//! Plays a recorded agent session to a provider's dialect and follows the
//! chat the way a client does, so that every provider is held to the same
//! account of what a conversation looks like.
//!
//! A recording is the agent's side of the socket, one message per line, as
//! the agent wrote it but for what names a machine or an account. The ids of
//! the host's own requests are not recorded, since they are the dialect's to
//! choose: an answer names the request it is for as `@<what was asked>`, and
//! is given the id of the oldest such request still unanswered.

use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::mpsc;

use super::super::{
    pane::Tag,
    session::{Action, Dialect, Out},
};
use super::*;

const PANE: &str = "%1";
const ADAPTER: &str = "played";

/// How one provider's wire names its requests and their answers.
pub(in crate::service::chat) trait Wire {
    /// What a message the host wrote asks for, and the id it asks under.
    fn asked(written: &Value) -> Option<(String, Value)>;
    /// Where a message of the agent's carries the id it answers.
    fn answers(inbound: &mut Value) -> Option<&mut Value>;
}

struct Stage<D: Dialect> {
    dialect: D,
    out: Out<D::Job>,
}

type Shared<D> = Arc<Mutex<Option<Stage<D>>>>;

struct Played<D: Dialect> {
    capabilities: v1::ChatCapabilities,
    dialect: Mutex<Option<D>>,
    shared: Shared<D>,
    actions: Mutex<Option<mpsc::UnboundedReceiver<Action<D::Job>>>>,
}

impl<D: Dialect> ChatProvider for Played<D> {
    fn adapter_id(&self) -> &'static str {
        ADAPTER
    }
    fn capabilities(&self) -> v1::ChatCapabilities {
        self.capabilities
    }
    fn load_history(&self, _pane_id: &str) -> Vec<v1::ChatItem> {
        Vec::new()
    }
    fn connect(&self, _pane_id: &str, sink: ChatSink) -> Box<dyn ChatSession> {
        let dialect = self.dialect.lock().unwrap().take().expect("one pane");
        let (out, actions) = Out::detached(sink);
        *self.actions.lock().unwrap() = Some(actions);
        *self.shared.lock().unwrap() = Some(Stage { dialect, out });
        Box::new(PlayedSession(self.shared.clone()))
    }
}

struct PlayedSession<D: Dialect>(Shared<D>);

impl<D: Dialect> ChatSession for PlayedSession<D> {
    fn command(&mut self, command: SessionCommand) -> Result<(), ChatError> {
        let mut stage = self.0.lock().unwrap();
        let Stage { dialect, out } = stage.as_mut().unwrap();
        dialect.command(command, out)
    }
}

/// The chat as a client holds it that has had the pane open from the start.
/// Applying an event checks what the wire promises about it.
#[derive(Default)]
pub(in crate::service::chat) struct Client {
    pub(in crate::service::chat) revision: u64,
    pub(in crate::service::chat) state: v1::ChatState,
    pub(in crate::service::chat) catalog: v1::ChatCatalog,
    pub(in crate::service::chat) items: Vec<v1::ChatItem>,
    /// How many snapshots have replaced what it held.
    pub(in crate::service::chat) snapshots: usize,
}

impl Client {
    fn apply(&mut self, event: v1::ChatEvent) {
        let change = event.change.expect("an event carries a change");
        if let Change::Snapshot(snapshot) = change {
            assert_eq!(event.revision, snapshot.revision);
            self.revision = snapshot.revision;
            self.state = snapshot.state.unwrap();
            self.catalog = snapshot.catalog.unwrap();
            assert!(snapshot.older_cursor.is_empty(), "the recordings are short");
            self.items = snapshot.items;
            self.snapshots += 1;
            return;
        }
        assert_eq!(event.revision, self.revision + 1, "a change was lost");
        self.revision = event.revision;
        match change {
            Change::Item(item) => {
                assert!(self.item(&item.id).is_none(), "{} is not new", item.id);
                self.items.push(item);
            }
            Change::Updated(item) => {
                let held = self.items.iter_mut().find(|held| held.id == item.id);
                let held = held.unwrap_or_else(|| panic!("{} is not held", item.id));
                assert_eq!(held.kind, item.kind, "{} changed kind", item.id);
                *held = item;
            }
            Change::Delta(delta) => {
                let held = self.items.iter_mut().find(|held| held.id == delta.item_id);
                let held = held.unwrap_or_else(|| panic!("{} is not held", delta.item_id));
                assert!(held.streaming, "{} is not streaming", held.id);
                held.text.push_str(&delta.text_append);
            }
            Change::State(state) => {
                let waiting = state.turn == i32::from(v1::ChatTurnState::Waiting);
                assert_eq!(state.pending.is_some(), waiting);
                self.state = state;
            }
            Change::Catalog(catalog) => self.catalog = catalog,
            Change::Truncate(truncate) => {
                let keep = match truncate.after_item_id.as_str() {
                    "" => 0,
                    id => 1 + self.items.iter().position(|item| item.id == id).unwrap(),
                };
                self.items.truncate(keep);
            }
            Change::Snapshot(_) => unreachable!(),
        }
        let mut ids: Vec<&str> = self.items.iter().map(|item| item.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), self.items.len(), "two items share an id");
    }

    pub(in crate::service::chat) fn item(&self, id: &str) -> Option<&v1::ChatItem> {
        self.items.iter().find(|item| item.id == id)
    }

    fn turn(&self) -> v1::ChatTurnState {
        v1::ChatTurnState::try_from(self.state.turn).unwrap()
    }

    /// The items after the one with `id`.
    fn after(&self, id: &str) -> &[v1::ChatItem] {
        let index = self.items.iter().position(|item| item.id == id);
        &self.items[index.unwrap_or_else(|| panic!("{id} is not held")) + 1..]
    }

    /// One line per item, for a failure to show.
    pub(in crate::service::chat) fn outline(&self) -> Vec<String> {
        let kind = |item: &v1::ChatItem| {
            v1::ChatItemKind::try_from(item.kind)
                .unwrap()
                .as_str_name()
                .trim_start_matches("CHAT_ITEM_KIND_")
                .to_lowercase()
        };
        self.items
            .iter()
            .map(|item| match &item.tool {
                Some(tool) => format!("{} {}: {}", item.id, tool.title, tool.detail),
                None => format!("{} {}: {}", item.id, kind(item), item.text),
            })
            .collect()
    }
}

// --- what every provider's recorded session must look like ------------------
//
// The recordings are of one conversation, held with each agent: a plain
// answer, a question answered "Blue", a shell command that needs approval,
// a long answer that is stopped, a rewind of the command's message, and a
// message after it.

impl Client {
    /// Nothing is under way: no turn, nothing streaming or running, nothing
    /// asked.
    pub(in crate::service::chat) fn assert_settled(&self) {
        assert_eq!(
            self.turn(),
            v1::ChatTurnState::Idle,
            "{:#?}",
            self.outline()
        );
        assert_eq!(
            self.state.connection,
            i32::from(v1::ChatConnection::Ready),
            "{}",
            self.state.failure
        );
        assert!(self.state.pending.is_none());
        for item in &self.items {
            assert!(!item.streaming, "{} is still streaming", item.id);
            let running = i32::from(v1::ChatToolStatus::Running);
            let status = item.tool.as_ref().map(|tool| tool.status);
            assert_ne!(status, Some(running), "{} is still running", item.id);
        }
    }

    /// The message `id` was answered in words, and the turn is over.
    pub(in crate::service::chat) fn assert_answered(&self, id: &str, text: &str, says: &str) {
        self.assert_settled();
        let user = self
            .item(id)
            .unwrap_or_else(|| panic!("{:#?}", self.outline()));
        assert_eq!(user.kind, i32::from(v1::ChatItemKind::User));
        assert_eq!(user.text, text);
        assert!(user.rewindable);
        let answer = self
            .after(id)
            .iter()
            .rev()
            .find(|item| item.kind == i32::from(v1::ChatItemKind::Assistant));
        let answer = answer.unwrap_or_else(|| panic!("{:#?}", self.outline()));
        assert!(
            answer.text.to_lowercase().contains(says),
            "{:?} does not say {says:?}",
            answer.text
        );
    }

    /// The turn waits on a question with choices, and the answer that picks
    /// the one labelled `label`.
    pub(in crate::service::chat) fn assert_asks(&self, label: &str) -> SessionCommand {
        assert_eq!(self.turn(), v1::ChatTurnState::Waiting);
        let pending = self.state.pending.clone().unwrap();
        assert_eq!(pending.kind, i32::from(v1::ChatRequestKind::Question));
        let [question] = pending.questions.as_slice() else {
            panic!("one question was asked, not {}", pending.questions.len());
        };
        assert!(!question.prompt.is_empty() && !question.header.is_empty());
        assert!(question.options.len() >= 2);
        let choice = question.options.iter().find(|option| option.label == label);
        SessionCommand::Answer {
            request_id: pending.request_id,
            answer: v1::ChatAnswer {
                answers: vec![v1::ChatQuestionAnswer {
                    question_id: question.id.clone(),
                    option_ids: vec![choice.expect("the choice is offered").id.clone()],
                    other_text: String::new(),
                }],
                ..Default::default()
            },
        }
    }

    /// The turn waits on leave to run `command`, and the answer that gives
    /// it.
    pub(in crate::service::chat) fn assert_asks_leave(&self, command: &str) -> SessionCommand {
        assert_eq!(self.turn(), v1::ChatTurnState::Waiting);
        let pending = self.state.pending.clone().unwrap();
        assert_eq!(pending.kind, i32::from(v1::ChatRequestKind::Permission));
        assert!(!pending.title.is_empty());
        assert!(pending.detail.contains(command), "{:?}", pending.detail);
        assert!(pending.questions.is_empty());
        SessionCommand::Answer {
            request_id: pending.request_id,
            answer: v1::ChatAnswer {
                allow: true,
                ..Default::default()
            },
        }
    }

    /// After the message `id`, `command` ran and succeeded.
    pub(in crate::service::chat) fn assert_ran(&self, id: &str, command: &str) {
        let ran = self.after(id).iter().find_map(|item| {
            let tool = item.tool.as_ref()?;
            (tool.kind == i32::from(v1::ChatToolKind::Command)).then_some(tool)
        });
        let ran = ran.unwrap_or_else(|| panic!("{:#?}", self.outline()));
        assert!(ran.detail.contains(command), "{:?}", ran.detail);
        assert_eq!(ran.title, "run");
        assert_eq!(ran.status, i32::from(v1::ChatToolStatus::Ok));
    }

    /// The answer to the message `id` was stopped part of the way: what had
    /// been said stays, and a notice says it was stopped.
    pub(in crate::service::chat) fn assert_stopped(&self, id: &str) {
        self.assert_settled();
        let after = self.after(id);
        let said = after
            .iter()
            .find(|item| item.kind == i32::from(v1::ChatItemKind::Assistant));
        assert!(said.is_some_and(|said| said.text.starts_with("1\n2\n3")));
        let notice = after.last().unwrap();
        assert_eq!(notice.kind, i32::from(v1::ChatItemKind::Notice));
        assert_eq!(notice.text, "Interrupted");
    }

    /// What a chat that has reached its agent says of itself.
    pub(in crate::service::chat) fn assert_describes_itself(&self) {
        let models = &self.catalog.models;
        assert!(models.len() >= 2);
        for model in models {
            assert!(!model.id.is_empty() && !model.label.is_empty());
            assert!(!model.efforts.is_empty());
        }
        assert!(models.iter().any(|model| model.supports_fast));
        let kind = |name: &str| {
            let command = self
                .catalog
                .commands
                .iter()
                .find(|command| command.name == name);
            v1::ChatCommandKind::try_from(command.expect(name).kind).unwrap()
        };
        assert_eq!(kind("release-notes"), v1::ChatCommandKind::Skill);
        assert_eq!(kind("btw"), v1::ChatCommandKind::Muxflow);
        assert_eq!(kind("side"), v1::ChatCommandKind::Muxflow);
        let options = self.state.options.clone().unwrap();
        assert!(models.iter().any(|model| model.id == options.model));
        assert!(!options.effort.is_empty());
        assert_ne!(options.permission, 0);
        assert_ne!(options.speed, 0);
        assert!(!self.state.native_session_id.is_empty());
        let usage = self.state.usage.unwrap();
        assert!(usage.used_tokens > 0 && usage.window_tokens > usage.used_tokens);
    }
}

pub(in crate::service::chat) struct Replay<D: Dialect> {
    pub(in crate::service::chat) runtime: ChatRuntime,
    connection: Connection,
    shared: Shared<D>,
    actions: mpsc::UnboundedReceiver<Action<D::Job>>,
    /// Every message the dialect wrote to its agent.
    pub(in crate::service::chat) written: Vec<Value>,
    /// How many of them the recording has answered.
    answered: Vec<bool>,
    /// The jobs it asked for, which nobody runs here.
    pub(in crate::service::chat) jobs: Vec<D::Job>,
    /// What it told the agent runtime of the agent's state.
    pub(in crate::service::chat) reports: Vec<Value>,
    pub(in crate::service::chat) reconnects: usize,
    /// The titles it had its pane given, in order.
    pub(in crate::service::chat) titles: Vec<String>,
    pub(in crate::service::chat) client: Client,
    _directory: tempfile::TempDir,
}

impl<D: Dialect + Wire> Replay<D> {
    pub(in crate::service::chat) fn new(dialect: D, capabilities: v1::ChatCapabilities) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let shared: Shared<D> = Arc::default();
        let provider = Arc::new(Played {
            capabilities,
            dialect: Mutex::new(Some(dialect)),
            shared: shared.clone(),
            actions: Mutex::new(None),
        });
        let runtime = ChatRuntime::new(
            directory.path().join("settings.json"),
            vec![provider.clone()],
        );
        let mut connection = Connection::new();
        let snapshot = connection.open(&runtime, PANE);
        let mut client = Client::default();
        client.apply(v1::ChatEvent {
            pane_id: PANE.into(),
            revision: snapshot.revision,
            change: Some(Change::Snapshot(snapshot)),
        });
        client.snapshots = 0;
        let actions = provider.actions.lock().unwrap().take().unwrap();
        Self {
            runtime,
            connection,
            shared,
            actions,
            written: Vec::new(),
            answered: Vec::new(),
            jobs: Vec::new(),
            reports: Vec::new(),
            reconnects: 0,
            titles: Vec::new(),
            client,
            _directory: directory,
        }
    }

    /// Runs one step of the dialect, then takes in what came of it.
    fn step<T>(&mut self, step: impl FnOnce(&mut D, &mut Out<D::Job>) -> T) -> T {
        let result = {
            let mut stage = self.shared.lock().unwrap();
            let Stage { dialect, out } = stage.as_mut().unwrap();
            let result = step(dialect, out);
            out.flush();
            result
        };
        self.settle();
        result
    }

    fn settle(&mut self) {
        while let Ok(action) = self.actions.try_recv() {
            match action {
                Action::Write(message) => {
                    self.written.push(serde_json::from_str(&message).unwrap());
                    self.answered.push(false);
                }
                Action::Job(job) => self.jobs.push(job),
                Action::Report(payload) => self.reports.push(payload),
                Action::Reconnect => self.reconnects += 1,
                Action::Title(title) => self.titles.push(title),
            }
        }
        while let Ok(message) = self.connection.events.try_recv() {
            if let Some(event) = chat_event(message) {
                self.client.apply(event);
            }
        }
    }

    pub(in crate::service::chat) fn connect(&mut self, tag: &Tag) {
        self.step(|dialect, out| dialect.connected(tag, out));
    }

    pub(in crate::service::chat) fn disconnect(&mut self) {
        self.step(|dialect, out| dialect.disconnected(out));
    }

    pub(in crate::service::chat) fn end(&mut self, failure: &str) {
        self.step(|dialect, out| dialect.ended(failure, out));
    }

    pub(in crate::service::chat) fn done(&mut self, done: D::Done) {
        self.step(|dialect, out| dialect.done(done, out));
    }

    /// One message from the agent.
    pub(in crate::service::chat) fn agent(&mut self, line: &str) {
        let mut message: Value = serde_json::from_str(line).unwrap();
        if let Some(id) = D::answers(&mut message)
            && let Some(asked) = id.as_str().and_then(|id| id.strip_prefix('@'))
        {
            let request = self
                .written
                .iter()
                .enumerate()
                .find_map(|(index, written)| {
                    let (what, id) = D::asked(written)?;
                    (what == asked && !self.answered[index]).then_some((index, id))
                });
            let (index, request_id) =
                request.unwrap_or_else(|| panic!("nothing asked for {asked} is unanswered"));
            self.answered[index] = true;
            *id = request_id;
        }
        let line = message.to_string();
        self.step(|dialect, out| dialect.message(&line, out));
    }

    /// The agent's messages up to and including the first that has `until`
    /// in it.
    pub(in crate::service::chat) fn play<'a>(
        &mut self,
        lines: &mut impl Iterator<Item = &'a str>,
        until: &str,
    ) {
        loop {
            let line = lines
                .next()
                .unwrap_or_else(|| panic!("the recording ended before {until}"));
            self.agent(line);
            if line.contains(until) {
                return;
            }
        }
    }

    /// The agent's messages up to, and not including, the first that has
    /// `before` in it.
    pub(in crate::service::chat) fn play_before<'a>(
        &mut self,
        lines: &mut std::iter::Peekable<impl Iterator<Item = &'a str>>,
        before: &str,
    ) {
        while let Some(line) = lines.next_if(|line| !line.contains(before)) {
            self.agent(line);
        }
        assert!(
            lines.peek().is_some(),
            "the recording ended before {before}"
        );
    }

    /// The command as a client sends it: through the runtime's checks.
    pub(in crate::service::chat) fn command(
        &mut self,
        command: SessionCommand,
    ) -> Result<(), ChatError> {
        let result = self.runtime.command(&pane(PANE), ADAPTER, command);
        self.step(|_, _| ());
        result
    }

    pub(in crate::service::chat) fn send(&mut self, id: &str, text: &str) {
        self.command(SessionCommand::Send {
            text: text.into(),
            item_id: id.into(),
        })
        .unwrap();
    }

    /// The last message written that asks for `what`.
    pub(in crate::service::chat) fn last_asked(&self, what: &str) -> &Value {
        let asked = |written: &&Value| D::asked(written).is_some_and(|(asked, _)| asked == what);
        self.written
            .iter()
            .rev()
            .find(asked)
            .unwrap_or_else(|| panic!("{what} was never asked"))
    }
}
