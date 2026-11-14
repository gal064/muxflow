//! One chat pane's link to its agent: the socket, the reconnects, and the
//! order things happen in. What the agent's messages mean is a [`Dialect`]'s
//! business; this file knows no agent.
//!
//! A session is one task. It connects to the socket the pane is tagged with
//! and then does one thing at a time, in the order asked: a message from the
//! agent, a message to it, or a job (a file read, a tmux call) that must not
//! run under a lock. Nothing is read from the agent while a job runs, so a
//! job that loads the conversation cannot be overtaken by what follows it.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use tmux_agent_protocol::v1;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::mpsc,
    time::Instant,
};

use super::{
    PaneKey,
    pane::{self, Tag},
    provider::{Change, ChatError, ChatSession, ChatSink, SessionCommand},
    websocket,
};

/// How long streamed text waits to go out with what follows it. A provider
/// streams a delta per token, and each one published alone would be an event
/// per token for every connection that has the chat open.
const FLUSH: Duration = Duration::from_millis(50);
const RETRY: Duration = Duration::from_millis(200);
/// How long an agent that was just started has to listen.
const START_WAIT: Duration = Duration::from_secs(45);
/// How long an agent that stopped answering has to be back. Its socket goes
/// with it, so this is only as long as a restart of the pipe would take.
const RECONNECT_WAIT: Duration = Duration::from_secs(4);
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// The longest line read from an agent. A longer one is skipped.
const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
/// The longest WebSocket message read from an agent: one may carry a whole
/// conversation. A longer one ends the connection.
const MAX_WEBSOCKET_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
/// A link that lasted this long was a working one.
const STABLE: Duration = Duration::from_secs(10);
/// The most times in a row the wait before looking for an agent again is
/// doubled, when its links keep ending early.
const MAX_BACKOFF_DOUBLINGS: u32 = 5;
/// Messages read from the agent and not yet handled.
const INBOUND_QUEUE: usize = 64;

pub(super) enum Framing {
    /// One JSON message per line, as `chat-pipe` carries an agent's stdout.
    Lines,
    WebSocket,
}

/// What one agent's messages mean. Every method runs under the session's
/// lock and must not block: slow work is a [`Dialect::Job`].
pub(super) trait Dialect: Sized + Send + 'static {
    const FRAMING: Framing;
    /// Work to do off the lock, and what comes of it.
    type Job: Send + 'static;
    type Done: Send + 'static;

    /// Runs a job, on a thread that may block.
    fn run(job: Self::Job) -> Self::Done;
    fn done(&mut self, done: Self::Done, out: &mut Out<Self::Job>);

    /// The agent is listening. `tag` is the pane's as it was just read.
    fn connected(&mut self, tag: &Tag, out: &mut Out<Self::Job>);
    fn message(&mut self, message: &str, out: &mut Out<Self::Job>);
    fn command(
        &mut self,
        command: SessionCommand,
        out: &mut Out<Self::Job>,
    ) -> Result<(), ChatError>;
    /// The link is gone and is being looked for again.
    fn disconnected(&mut self, out: &mut Out<Self::Job>);
    /// The agent is gone for good: `failure` is empty when it had been
    /// reached and then ended, and says why when it never was.
    fn ended(&mut self, failure: &str, out: &mut Out<Self::Job>);
}

pub(super) enum Action<J> {
    Write(String),
    Job(J),
    /// Tell the agent runtime what the chat knows about the agent's state.
    Report(serde_json::Value),
    /// The pane's process was replaced: connect to the new one.
    Reconnect,
}

/// Where a dialect puts what it has to say.
pub(super) struct Out<J> {
    sink: ChatSink,
    actions: mpsc::UnboundedSender<Action<J>>,
    /// Streamed text not yet published, in order.
    deltas: Vec<v1::ChatItemDelta>,
}

impl<J> Out<J> {
    /// Publishes a change. Streamed text is held back briefly and joined;
    /// anything else goes out now, behind the text that came before it.
    pub(super) fn publish(&mut self, change: Change) {
        match change {
            Change::Delta(delta) => match self.deltas.last_mut() {
                Some(last) if last.item_id == delta.item_id => {
                    last.text_append.push_str(&delta.text_append);
                }
                _ => self.deltas.push(delta),
            },
            change => {
                self.flush();
                self.sink.publish(change);
            }
        }
    }

    /// Replaces the timeline with the conversation as it was read.
    pub(super) fn reset(&mut self, items: Vec<v1::ChatItem>, complete: bool) {
        self.deltas.clear();
        self.sink.reset(items, complete);
    }

    pub(super) fn flush(&mut self) {
        for delta in self.deltas.drain(..) {
            self.sink.publish(Change::Delta(delta));
        }
    }

    /// An `Out` with no session behind it: what it is asked to do comes out
    /// of the returned queue instead of being done.
    #[cfg(test)]
    pub(super) fn detached(sink: ChatSink) -> (Self, mpsc::UnboundedReceiver<Action<J>>) {
        let (actions, asked) = mpsc::unbounded_channel();
        let out = Self {
            sink,
            actions,
            deltas: Vec::new(),
        };
        (out, asked)
    }

    /// Sends one message to the agent. Dropped when there is no link.
    pub(super) fn send(&mut self, message: String) {
        let _ = self.actions.send(Action::Write(message));
    }

    pub(super) fn job(&mut self, job: J) {
        let _ = self.actions.send(Action::Job(job));
    }

    pub(super) fn reconnect(&mut self) {
        let _ = self.actions.send(Action::Reconnect);
    }

    /// Tells the agent runtime of a state no hook of the agent reports, as
    /// the hook that would have: `payload` is that hook's.
    pub(super) fn report(&mut self, payload: serde_json::Value) {
        let _ = self.actions.send(Action::Report(payload));
    }

    pub(super) fn pane_id(&self) -> &str {
        &self.sink.key().pane_id
    }
}

struct Shared<D: Dialect> {
    dialect: D,
    out: Out<D::Job>,
    /// False once the session is dropped: nothing may publish after that.
    live: bool,
}

pub(super) struct Session<D: Dialect> {
    shared: Arc<Mutex<Shared<D>>>,
    task: tokio::task::JoinHandle<()>,
}

impl<D: Dialect> Session<D> {
    /// Starts the session's task. Must be called inside the tokio runtime.
    pub(super) fn start(adapter_id: &'static str, dialect: D, sink: ChatSink) -> Self {
        let (actions, queued) = mpsc::unbounded_channel();
        let key = sink.key().clone();
        let shared = Arc::new(Mutex::new(Shared {
            dialect,
            out: Out {
                sink,
                actions,
                deltas: Vec::new(),
            },
            live: true,
        }));
        let task = tokio::spawn(drive(shared.clone(), queued, key, adapter_id));
        Self { shared, task }
    }
}

impl<D: Dialect> ChatSession for Session<D> {
    fn command(&mut self, command: SessionCommand) -> Result<(), ChatError> {
        let mut shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        let Shared { dialect, out, .. } = &mut *shared;
        dialect.command(command, out)
    }
}

impl<D: Dialect> Drop for Session<D> {
    fn drop(&mut self) {
        self.shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .live = false;
        self.task.abort();
    }
}

/// Runs `step` on the dialect. `None` once the session has been dropped.
fn with<D: Dialect, T>(
    shared: &Mutex<Shared<D>>,
    step: impl FnOnce(&mut D, &mut Out<D::Job>) -> T,
) -> Option<(T, bool)> {
    let mut shared = shared.lock().unwrap_or_else(PoisonError::into_inner);
    if !shared.live {
        return None;
    }
    let Shared { dialect, out, .. } = &mut *shared;
    let result = step(dialect, out);
    Some((result, !out.deltas.is_empty()))
}

enum Inbound {
    Message(String),
    Ping(Vec<u8>),
}

struct Link {
    inbound: mpsc::Receiver<Inbound>,
    writer: OwnedWriteHalf,
    reader: tokio::task::JoinHandle<()>,
    websocket: bool,
}

impl Drop for Link {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl Link {
    async fn open(socket: &std::path::Path, framing: &Framing) -> std::io::Result<Self> {
        let mut stream = UnixStream::connect(socket).await?;
        let websocket = matches!(framing, Framing::WebSocket);
        if websocket {
            tokio::time::timeout(WRITE_TIMEOUT, websocket::handshake(&mut stream))
                .await
                .map_err(|_| std::io::Error::other("the handshake timed out"))??;
        }
        let (read, writer) = stream.into_split();
        let (sender, inbound) = mpsc::channel(INBOUND_QUEUE);
        let reader = if websocket {
            tokio::spawn(read_websocket(read, sender))
        } else {
            tokio::spawn(read_lines(read, sender))
        };
        Ok(Self {
            inbound,
            writer,
            reader,
            websocket,
        })
    }

    async fn write(&mut self, bytes: &[u8]) -> bool {
        matches!(
            tokio::time::timeout(WRITE_TIMEOUT, self.writer.write_all(bytes)).await,
            Ok(Ok(()))
        )
    }

    async fn send(&mut self, message: &str) -> bool {
        if self.websocket {
            self.write(&websocket::text(message)).await
        } else {
            let mut line = Vec::with_capacity(message.len() + 1);
            line.extend(message.as_bytes());
            line.push(b'\n');
            self.write(&line).await
        }
    }
}

async fn read_lines(mut read: OwnedReadHalf, sender: mpsc::Sender<Inbound>) {
    let mut chunk = vec![0_u8; 64 * 1024];
    let mut line = Vec::new();
    // The line being read is past the bound: its rest is skipped.
    let mut skipping = false;
    loop {
        let count = match read.read(&mut chunk).await {
            Ok(count) if count > 0 => count,
            _ => return,
        };
        for piece in chunk[..count].split_inclusive(|byte| *byte == b'\n') {
            let ends = piece.last() == Some(&b'\n');
            if !skipping {
                line.extend(piece);
                if line.len() > MAX_MESSAGE_BYTES {
                    line = Vec::new();
                    skipping = true;
                }
            }
            if !ends {
                continue;
            }
            if !std::mem::take(&mut skipping) {
                let message = String::from_utf8_lossy(&line).trim_end().to_owned();
                line.clear();
                if !message.is_empty() && sender.send(Inbound::Message(message)).await.is_err() {
                    return;
                }
            }
        }
    }
}

async fn read_websocket(mut read: OwnedReadHalf, sender: mpsc::Sender<Inbound>) {
    let mut chunk = vec![0_u8; 64 * 1024];
    let mut decoder = websocket::Decoder::new(MAX_WEBSOCKET_MESSAGE_BYTES);
    loop {
        let count = match read.read(&mut chunk).await {
            Ok(count) if count > 0 => count,
            _ => return,
        };
        decoder.push(&chunk[..count]);
        loop {
            let inbound = match decoder.next() {
                Ok(Some(websocket::Frame::Text(message))) => Inbound::Message(message),
                Ok(Some(websocket::Frame::Ping(payload))) => Inbound::Ping(payload),
                Ok(None) => break,
                Ok(Some(websocket::Frame::Close)) | Err(_) => return,
            };
            if sender.send(inbound).await.is_err() {
                return;
            }
        }
    }
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(work).await.ok()
}

/// Hands the agent runtime a state the chat knows of, the way a hook of the
/// agent's would have.
fn report(key: &PaneKey, adapter_id: &str, payload: &serde_json::Value) {
    let event = v1::AgentHookEvent {
        adapter_id: adapter_id.to_owned(),
        source_event_id: format!("chat-{}", uuid::Uuid::new_v4()),
        native_session_id: payload["session_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        pane_id: key.pane_id.clone(),
        payload_json: payload.to_string().into_bytes(),
        occurred_at_unix_millis: super::now_unix_millis(),
        origin_server_identity: key.server_identity.clone(),
        ..Default::default()
    };
    let _ = crate::service::agents::AgentRuntime::global().ingest_live_hook(&event);
}

enum Wait {
    /// The pane's process was just started, or replaced.
    Start,
    /// The link was lost.
    Back,
}

/// Does what was asked for while there is no agent: a message to it is lost
/// with the link it was for; a job still runs. `None` once the session has
/// been dropped.
async fn without_link<D: Dialect>(
    action: Action<D::Job>,
    shared: &Mutex<Shared<D>>,
    key: &PaneKey,
    adapter_id: &'static str,
) -> Option<()> {
    match action {
        Action::Job(job) => {
            if let Some(done) = blocking(move || D::run(job)).await {
                with(shared, |dialect, out| dialect.done(done, out))?;
            }
        }
        Action::Report(payload) => {
            let key = key.clone();
            blocking(move || report(&key, adapter_id, &payload)).await;
        }
        Action::Write(_) | Action::Reconnect => {}
    }
    Some(())
}

async fn drive<D: Dialect>(
    shared: Arc<Mutex<Shared<D>>>,
    mut actions: mpsc::UnboundedReceiver<Action<D::Job>>,
    key: PaneKey,
    adapter_id: &'static str,
) {
    let mut wait = Wait::Start;
    let mut reached = false;
    // How many links in a row ended early.
    let mut short_links = 0_u32;
    loop {
        // Look for the agent.
        let deadline = Instant::now()
            + match wait {
                Wait::Start => START_WAIT,
                Wait::Back => RECONNECT_WAIT,
            };
        let mut socket = PathBuf::new();
        // The socket was there: once it is not, what made it has gone.
        let mut listened = false;
        let found = loop {
            let pane_id = key.pane_id.clone();
            let tag = blocking(move || pane::read(&pane_id)).await.flatten();
            let Some(tag) = tag.filter(|tag| tag.adapter == adapter_id) else {
                break None;
            };
            socket.clone_from(&tag.socket);
            if let Ok(link) = Link::open(&tag.socket, &D::FRAMING).await {
                break Some((tag, link));
            }
            let listens = std::fs::symlink_metadata(&tag.socket).is_ok();
            if Instant::now() >= deadline || (listened && !listens) {
                break None;
            }
            listened |= listens;
            let retry = tokio::time::sleep(RETRY);
            tokio::pin!(retry);
            loop {
                tokio::select! {
                    () = &mut retry => break,
                    action = actions.recv() => {
                        let Some(action) = action else { return };
                        if without_link(action, &shared, &key, adapter_id).await.is_none() {
                            return;
                        }
                    }
                }
            }
        };
        let Some((tag, mut link)) = found else {
            let failure = if reached {
                ""
            } else {
                "The agent did not start as a chat."
            };
            if with(&shared, |dialect, out| {
                dialect.ended(failure, out);
                out.flush();
            })
            .is_none()
            {
                return;
            }
            // The pane is a terminal again, unless it has been made a chat
            // anew meanwhile, which is then another session's.
            let pane_id = key.pane_id.clone();
            blocking(move || {
                if pane::read(&pane_id).is_some_and(|tag| tag.socket == socket) {
                    pane::untag(&pane_id);
                }
                if socket.starts_with(pane::directory()) {
                    let _ = std::fs::remove_file(&socket);
                    // What the agent was to start with, if nothing read it.
                    let _ = std::fs::remove_file(socket.with_extension("env"));
                }
            })
            .await;
            super::topology_changed();
            return;
        };
        reached = true;
        wait = Wait::Back;
        // What was asked for before this link was for the one before it.
        while let Ok(action) = actions.try_recv() {
            if without_link(action, &shared, &key, adapter_id)
                .await
                .is_none()
            {
                return;
            }
        }
        if with(&shared, |dialect, out| dialect.connected(&tag, out)).is_none() {
            return;
        }
        let linked = Instant::now();

        // Talk to it, one thing at a time.
        let mut flush_at: Option<Instant> = None;
        loop {
            let step = tokio::select! {
                biased;
                action = actions.recv() => match action {
                    Some(Action::Write(message)) => {
                        if !link.send(&message).await {
                            break;
                        }
                        Some(((), false))
                    }
                    Some(Action::Job(job)) => match blocking(move || D::run(job)).await {
                        Some(done) => with(&shared, |dialect, out| dialect.done(done, out)),
                        None => Some(((), false)),
                    },
                    Some(Action::Report(payload)) => {
                        let key = key.clone();
                        blocking(move || report(&key, adapter_id, &payload)).await;
                        Some(((), false))
                    }
                    Some(Action::Reconnect) => {
                        wait = Wait::Start;
                        break;
                    }
                    None => return,
                },
                inbound = link.inbound.recv() => match inbound {
                    Some(Inbound::Message(message)) => {
                        with(&shared, |dialect, out| dialect.message(&message, out))
                    }
                    Some(Inbound::Ping(payload)) => {
                        if !link.write(&websocket::pong(&payload)).await {
                            break;
                        }
                        Some(((), false))
                    }
                    None => break,
                },
                () = async {
                    match flush_at {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => {
                    flush_at = None;
                    with(&shared, |_, out| out.flush())
                }
            };
            match step {
                Some(((), true)) => {
                    flush_at.get_or_insert_with(|| Instant::now() + FLUSH);
                }
                Some(((), false)) => {}
                None => return,
            }
        }
        drop(link);
        if with(&shared, |dialect, out| {
            dialect.disconnected(out);
            out.flush();
        })
        .is_none()
        {
            return;
        }
        // An agent whose links keep ending early is asked less and less often.
        if matches!(wait, Wait::Start) || linked.elapsed() >= STABLE {
            short_links = 0;
        } else {
            tokio::time::sleep(RETRY * 2_u32.pow(short_links)).await;
            short_links = (short_links + 1).min(MAX_BACKOFF_DOUBLINGS);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lines_are_read_whole_and_one_past_the_bound_is_skipped() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("s.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let client = UnixStream::connect(&path).await.unwrap();
        let (mut server, _) = listener.accept().await.unwrap();
        let (read, _write) = client.into_split();
        let (sender, mut inbound) = mpsc::channel(8);
        tokio::spawn(read_lines(read, sender));

        tokio::spawn(async move {
            server.write_all(b"{\"a\":1}\n\n{\"b\":").await.unwrap();
            server.write_all(b"2}\r\n").await.unwrap();
            let long = vec![b'x'; MAX_MESSAGE_BYTES + 10];
            server.write_all(&long).await.unwrap();
            server.write_all(b"\n{\"c\":3}\n").await.unwrap();
        });
        let mut messages = Vec::new();
        while let Some(Inbound::Message(message)) = inbound.recv().await {
            messages.push(message);
        }
        assert_eq!(messages, ["{\"a\":1}", "{\"b\":2}", "{\"c\":3}"]);
    }
}
