use std::time::Duration;

use tmux_agent_protocol::{envelope, read_frame, v1::envelope::Payload, write_frame};
use tokio::{net::UnixStream, sync::mpsc, time::timeout};

use super::super::events::{
    ControlEventRegistration, SequencerControl, register_control_event_sink,
};
use super::scripted::ScriptedProvider;
use super::*;

const WAIT: Duration = Duration::from_secs(5);

// --- an isolated runtime, with connections that are only an event queue -----

fn scripted_runtime(directory: &tempfile::TempDir) -> ChatRuntime {
    ChatRuntime::new(
        directory.path().join("settings.json"),
        vec![Arc::new(ScriptedProvider::new(Duration::ZERO))],
    )
}

/// The pane `pane_id` of the one tmux server these tests pretend to have.
fn pane(pane_id: &str) -> PaneKey {
    PaneKey {
        server_identity: "chat-tests".into(),
        pane_id: pane_id.into(),
    }
}

/// The hub these connections register in is the process's, so another test's
/// broadcast can land in their queues. Only chat events count here.
fn chat_event(message: SequencerControl) -> Option<v1::ChatEvent> {
    match message {
        SequencerControl::OrderedEvent(event) if event.kind == i32::from(v1::EventKind::Chat) => {
            event.chat
        }
        _ => None,
    }
}

struct Connection {
    id: u64,
    sender: mpsc::Sender<SequencerControl>,
    events: mpsc::Receiver<SequencerControl>,
    _registration: ControlEventRegistration,
}

impl Connection {
    fn new() -> Self {
        Self::with_queue(4096)
    }

    fn with_queue(capacity: usize) -> Self {
        let (sender, events) = mpsc::channel(capacity);
        let registration = register_control_event_sink(sender.clone());
        Self {
            id: registration.id,
            sender,
            events,
            _registration: registration,
        }
    }

    /// Opens the pane with the runtime's one provider and returns the
    /// snapshot, which is already queued when `open` returns. Events still
    /// waiting from before it are passed over.
    fn open(&mut self, runtime: &ChatRuntime, pane_id: &str) -> v1::ChatSnapshot {
        let adapter_id = runtime.providers.keys().next().copied().unwrap();
        runtime.open(self.id, &pane(pane_id), adapter_id).unwrap();
        loop {
            let message = self.events.try_recv().expect("OPEN queued no snapshot");
            let Some(event) = chat_event(message) else {
                continue;
            };
            if let Some(Change::Snapshot(snapshot)) = event.change {
                assert_eq!(
                    (event.pane_id.as_str(), event.revision),
                    (pane_id, snapshot.revision)
                );
                return snapshot;
            }
        }
    }

    /// The pane's chat events, in order, up to and including the first that
    /// `last` accepts.
    async fn until(&mut self, last: impl Fn(&Change) -> bool) -> Vec<v1::ChatEvent> {
        let mut events = Vec::new();
        loop {
            let message = timeout(WAIT, self.events.recv())
                .await
                .expect("timed out waiting for a chat event")
                .expect("the connection's queue closed");
            let Some(event) = chat_event(message) else {
                continue;
            };
            let done = last(event.change.as_ref().unwrap());
            events.push(event);
            if done {
                return events;
            }
        }
    }

    /// No chat event is waiting.
    fn is_quiet(&mut self) -> bool {
        std::iter::from_fn(|| self.events.try_recv().ok())
            .all(|message| chat_event(message).is_none())
    }
}

fn turn_is(turn: v1::ChatTurnState) -> impl Fn(&Change) -> bool {
    move |change| matches!(change, Change::State(state) if state.turn == i32::from(turn))
}

fn send(runtime: &ChatRuntime, pane_id: &str, text: &str, item_id: &str) -> Result<(), ChatError> {
    runtime.command(
        &pane(pane_id),
        scripted::ADAPTER_ID,
        SessionCommand::Send {
            text: text.into(),
            item_id: item_id.into(),
        },
    )
}

/// A rewind as the dispatcher hands it over: the runtime finds the item before.
fn rewinding(item_id: &str) -> SessionCommand {
    SessionCommand::Rewind {
        item_id: item_id.into(),
        after_item_id: String::new(),
    }
}

fn command(runtime: &ChatRuntime, pane_id: &str, command: SessionCommand) -> Result<(), ChatError> {
    runtime.command(&pane(pane_id), scripted::ADAPTER_ID, command)
}

/// One word per event, so a whole turn reads as a line.
fn outline(events: &[v1::ChatEvent]) -> Vec<String> {
    events
        .iter()
        .map(|event| match event.change.as_ref().unwrap() {
            Change::Item(item) | Change::Updated(item) => match &item.tool {
                Some(tool) => format!(
                    "{}:{}",
                    tool.title,
                    v1::ChatToolStatus::try_from(tool.status)
                        .unwrap()
                        .as_str_name()
                        .trim_start_matches("CHAT_TOOL_STATUS_")
                        .to_lowercase()
                ),
                None => format!(
                    "{}{}",
                    v1::ChatItemKind::try_from(item.kind)
                        .unwrap()
                        .as_str_name()
                        .trim_start_matches("CHAT_ITEM_KIND_")
                        .to_lowercase(),
                    if item.streaming { ":streaming" } else { "" }
                ),
            },
            Change::Delta(_) => "delta".into(),
            Change::State(state) => v1::ChatTurnState::try_from(state.turn)
                .unwrap()
                .as_str_name()
                .trim_start_matches("CHAT_TURN_STATE_")
                .into(),
            Change::Catalog(_) => "catalog".into(),
            Change::Truncate(_) => "truncate".into(),
            Change::Snapshot(_) => "snapshot".into(),
        })
        .collect()
}

fn assert_consecutive(after: u64, events: &[v1::ChatEvent]) {
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event.revision, after + 1 + index as u64);
    }
}

fn pending(events: &[v1::ChatEvent]) -> v1::ChatPendingRequest {
    match events.last().unwrap().change.as_ref().unwrap() {
        Change::State(state) => state.pending.clone().expect("a pending request"),
        _ => panic!("the turn did not stop on a request"),
    }
}

// --- the scripted provider through the runtime ------------------------------

#[tokio::test]
async fn a_new_chat_opens_on_the_newest_page_of_a_finished_transcript() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = scripted_runtime(&directory);
    let snapshot = Connection::new().open(&runtime, "%1");

    let state = snapshot.state.unwrap();
    assert_eq!(
        (state.pane_id.as_str(), state.adapter_id.as_str()),
        ("%1", "scripted")
    );
    assert_eq!(state.turn, i32::from(v1::ChatTurnState::Idle));
    assert_eq!(state.connection, i32::from(v1::ChatConnection::Ready));
    assert!(state.history_complete && state.pending.is_none());
    assert_eq!(state.usage.unwrap().window_tokens, 1_000_000);

    let catalog = snapshot.catalog.unwrap();
    assert!(catalog.capabilities.unwrap().send_mid_turn);
    assert_eq!(catalog.models.len(), 3);
    assert!(catalog.models[0].supports_fast && catalog.models[0].efforts.len() == 5);
    let kinds: Vec<i32> = catalog
        .commands
        .iter()
        .map(|command| command.kind)
        .collect();
    for kind in [
        v1::ChatCommandKind::Command,
        v1::ChatCommandKind::Skill,
        v1::ChatCommandKind::Muxflow,
    ] {
        assert!(kinds.contains(&i32::from(kind)));
    }

    // The transcript is longer than a page, and its tail shows every kind.
    assert_eq!(snapshot.items.len(), MAX_PAGE_ITEMS);
    assert_eq!(snapshot.older_cursor, snapshot.items[0].id);
    let tools: Vec<&v1::ChatTool> = snapshot
        .items
        .iter()
        .filter_map(|item| item.tool.as_ref())
        .collect();
    for kind in 1..=8 {
        assert!(
            tools.iter().any(|tool| tool.kind == kind),
            "tool kind {kind}"
        );
    }
    let edit = tools
        .iter()
        .find(|tool| tool.kind == i32::from(v1::ChatToolKind::Edit))
        .unwrap();
    assert_eq!((edit.added, edit.removed), (3, 1));
    assert!(edit.path.ends_with("links.rs"));
    assert!(tools.iter().any(|tool| {
        tool.kind == i32::from(v1::ChatToolKind::Command) && !tool.output_preview.is_empty()
    }));
    for kind in 1..=5 {
        assert!(
            snapshot.items.iter().any(|item| item.kind == kind),
            "item kind {kind}"
        );
    }
}

#[tokio::test]
async fn a_question_turn_streams_stops_on_two_questions_and_finishes_when_answered() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = scripted_runtime(&directory);
    let mut connection = Connection::new();
    let snapshot = connection.open(&runtime, "%1");

    send(&runtime, "%1", "Fix the wrapped paths", "m1").unwrap();
    let events = connection.until(turn_is(v1::ChatTurnState::Waiting)).await;
    assert_consecutive(snapshot.revision, &events);
    assert_eq!(
        outline(&events),
        [
            "user",
            "WORKING",
            "reasoning",
            "read:running",
            "read:ok",
            "run:running",
            "run:ok",
            "WORKING",
            "assistant:streaming",
            "delta",
            "delta",
            "delta",
            "assistant",
            "WAITING",
        ]
    );
    let Some(Change::Item(user)) = &events[0].change else {
        panic!("the first event is the user's message");
    };
    assert_eq!((user.id.as_str(), user.rewindable), ("m1", true));
    // An item arrives once as new and after that as an update of itself.
    let updates: Vec<usize> = (0..events.len())
        .filter(|index| matches!(events[*index].change, Some(Change::Updated(_))))
        .collect();
    assert_eq!(updates, [4, 6, 12]);
    let request = pending(&events);
    assert_eq!(request.kind, i32::from(v1::ChatRequestKind::Question));
    assert_eq!(request.questions.len(), 2);
    assert!(request.questions[0].allow_other && !request.questions[0].multi_select);
    assert!(request.questions[1].multi_select && !request.questions[1].allow_other);

    let waiting = events.last().unwrap().revision;
    command(
        &runtime,
        "%1",
        SessionCommand::Answer {
            request_id: request.request_id.clone(),
            answer: v1::ChatAnswer {
                answers: vec![
                    v1::ChatQuestionAnswer {
                        question_id: "scope".into(),
                        other_text: "Only in tables".into(),
                        ..Default::default()
                    },
                    v1::ChatQuestionAnswer {
                        question_id: "checks".into(),
                        option_ids: vec!["tests".into(), "clippy".into()],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        },
    )
    .unwrap();
    let events = connection.until(turn_is(v1::ChatTurnState::Idle)).await;
    assert_consecutive(waiting, &events);
    assert_eq!(
        outline(&events),
        [
            "WORKING",
            "edit:running",
            "edit:ok",
            "assistant",
            "WORKING",
            "IDLE"
        ]
    );
    let Some(Change::Item(reply)) = &events[3].change else {
        panic!("expected the closing message");
    };
    assert!(reply.text.contains("Only in tables, Unit tests, Clippy"));
    let Some(Change::State(idle)) = &events[5].change else {
        panic!("expected the idle state");
    };
    assert!(idle.pending.is_none());
    assert_eq!(idle.usage.unwrap().used_tokens, 209_000 + 1_500 + 4_000);

    // The answered request is gone, so answering it again is refused.
    let again = command(
        &runtime,
        "%1",
        SessionCommand::Answer {
            request_id: request.request_id,
            answer: v1::ChatAnswer::default(),
        },
    );
    assert_eq!(again.unwrap_err().code, "chat_request_not_pending");
}

#[tokio::test]
async fn the_second_turn_asks_permission_and_a_denial_fails_the_command() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = scripted_runtime(&directory);
    let mut connection = Connection::new();
    connection.open(&runtime, "%1");

    send(&runtime, "%1", "first", "").unwrap();
    let events = connection.until(turn_is(v1::ChatTurnState::Waiting)).await;
    let skip = SessionCommand::Answer {
        request_id: pending(&events).request_id,
        answer: v1::ChatAnswer {
            skipped: true,
            ..Default::default()
        },
    };
    command(&runtime, "%1", skip).unwrap();
    connection.until(turn_is(v1::ChatTurnState::Idle)).await;

    send(&runtime, "%1", "second", "").unwrap();
    let events = connection.until(turn_is(v1::ChatTurnState::Waiting)).await;
    let request = pending(&events);
    assert_eq!(request.kind, i32::from(v1::ChatRequestKind::Permission));
    assert!(request.detail.starts_with("git push"));
    assert!(request.questions.is_empty());

    let deny = SessionCommand::Answer {
        request_id: request.request_id,
        answer: v1::ChatAnswer::default(),
    };
    command(&runtime, "%1", deny).unwrap();
    let events = connection.until(turn_is(v1::ChatTurnState::Idle)).await;
    assert_eq!(
        outline(&events),
        [
            "WORKING",
            "run:running",
            "run:failed",
            "assistant",
            "WORKING",
            "IDLE"
        ]
    );
}

#[tokio::test]
async fn interrupt_ends_a_turn_with_nothing_left_running_or_pending() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = scripted_runtime(&directory);
    let mut connection = Connection::new();
    connection.open(&runtime, "%1");

    send(&runtime, "%1", "go", "").unwrap();
    connection.until(turn_is(v1::ChatTurnState::Waiting)).await;
    // A message sent mid-turn joins the turn instead of starting another.
    send(&runtime, "%1", "and also this", "m2").unwrap();
    command(&runtime, "%1", SessionCommand::Interrupt).unwrap();
    let events = connection.until(turn_is(v1::ChatTurnState::Idle)).await;
    assert_eq!(outline(&events), ["user", "notice", "IDLE"]);
    let Some(Change::State(state)) = &events[2].change else {
        panic!("expected the idle state");
    };
    assert!(state.pending.is_none());

    // Interrupting an idle chat changes nothing.
    command(&runtime, "%1", SessionCommand::Interrupt).unwrap();
    assert!(connection.is_quiet());
}

#[tokio::test]
async fn stopping_a_turn_mid_step_finishes_what_was_running_or_streaming() {
    // A real pause between steps, so the turn can be caught part-way: the
    // script cannot take its next step until this test waits again.
    let directory = tempfile::tempdir().unwrap();
    let runtime = ChatRuntime::new(
        directory.path().join("settings.json"),
        vec![Arc::new(ScriptedProvider::new(Duration::from_millis(5)))],
    );
    let mut connection = Connection::new();
    connection.open(&runtime, "%1");
    let running = |change: &Change| {
        let running = i32::from(v1::ChatToolStatus::Running);
        matches!(change, Change::Item(item)
            if item.tool.as_ref().is_some_and(|tool| tool.title == "run" && tool.status == running))
    };

    // Interrupted while a command is running: the command fails, stopped.
    send(&runtime, "%1", "first", "m1").unwrap();
    connection.until(running).await;
    command(&runtime, "%1", SessionCommand::Interrupt).unwrap();
    let events = connection.until(turn_is(v1::ChatTurnState::Idle)).await;
    assert_eq!(outline(&events), ["run:failed", "notice", "IDLE"]);
    let Some(Change::Updated(stopped)) = &events[0].change else {
        unreachable!();
    };
    assert_eq!(stopped.tool.as_ref().unwrap().result_summary, "stopped");

    // Rewound while the reply is streaming: the reply is closed with the text
    // it had, the turn ends, and then the message and all after it go.
    send(&runtime, "%1", "second", "m2").unwrap();
    connection
        .until(|change| matches!(change, Change::Delta(_)))
        .await;
    command(&runtime, "%1", rewinding("m2")).unwrap();
    let events = connection
        .until(|change| matches!(change, Change::Truncate(_)))
        .await;
    assert_eq!(outline(&events), ["assistant", "IDLE", "truncate"]);
    let Some(Change::Updated(closed)) = &events[0].change else {
        unreachable!();
    };
    assert_eq!(closed.text, "The branch has to be pushed ");

    // Nothing of either turn is left running, streaming or pending, and no
    // step of a stopped turn arrives late.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(connection.is_quiet());
    let snapshot = connection.open(&runtime, "%1");
    assert!(snapshot.state.unwrap().pending.is_none());
    assert_eq!(snapshot.items.last().unwrap().id, "turn-1-stopped");
    let running = i32::from(v1::ChatToolStatus::Running);
    assert!(snapshot.items.iter().all(|item| {
        !item.streaming && item.tool.as_ref().is_none_or(|tool| tool.status != running)
    }));
}

#[tokio::test]
async fn rewind_drops_the_message_and_what_followed_and_only_user_messages_rewind() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = scripted_runtime(&directory);
    let mut connection = Connection::new();
    let snapshot = connection.open(&runtime, "%1");
    let last_before = snapshot.items.last().unwrap().id.clone();

    send(&runtime, "%1", "go", "m1").unwrap();
    connection.until(turn_is(v1::ChatTurnState::Waiting)).await;

    let rewind = |item_id: &str| command(&runtime, "%1", rewinding(item_id));
    for not_rewindable in ["turn-1-read", "missing"] {
        assert_eq!(
            rewind(not_rewindable).unwrap_err().code,
            "chat_item_not_rewindable"
        );
    }
    rewind("m1").unwrap();
    let events = connection
        .until(|change| matches!(change, Change::Truncate(_)))
        .await;
    // The waiting turn ends first; then everything from the message on goes.
    assert_eq!(outline(&events), ["IDLE", "truncate"]);
    let Some(Change::Truncate(truncate)) = &events[1].change else {
        unreachable!();
    };
    assert_eq!(truncate.after_item_id, last_before);
    let reopened = connection.open(&runtime, "%1");
    assert_eq!(reopened.items.last().unwrap().id, last_before);
    assert_eq!(reopened.revision, events[1].revision);

    // Rewinding the very first message empties the timeline.
    let directory = tempfile::tempdir().unwrap();
    let runtime = scripted_runtime(&directory);
    let mut connection = Connection::new();
    connection.open(&runtime, "%2");
    command(&runtime, "%2", rewinding("history-q001")).unwrap();
    let events = connection
        .until(|change| matches!(change, Change::Truncate(_)))
        .await;
    let Some(Change::Truncate(truncate)) = &events.last().unwrap().change else {
        unreachable!();
    };
    assert_eq!(truncate.after_item_id, "");
    assert!(connection.open(&runtime, "%2").items.is_empty());
}

#[tokio::test]
async fn options_merge_into_the_current_ones_and_stay_valid_for_the_model() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = scripted_runtime(&directory);
    let mut connection = Connection::new();
    connection.open(&runtime, "%1");
    let mut set = async |options: v1::ChatOptions| {
        command(&runtime, "%1", SessionCommand::SetOptions(options))?;
        let events = connection
            .until(|change| matches!(change, Change::State(_)))
            .await;
        let Some(Change::State(state)) = events.into_iter().next().unwrap().change else {
            unreachable!();
        };
        Ok::<_, ChatError>(state.options.unwrap())
    };

    let fast = set(v1::ChatOptions {
        speed: v1::ChatSpeed::Fast.into(),
        permission: v1::ChatPermissionMode::Full.into(),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(
        (fast.model.as_str(), fast.effort.as_str()),
        ("opus-5-5", "high")
    );
    assert_eq!(fast.speed, i32::from(v1::ChatSpeed::Fast));
    assert_eq!(fast.permission, i32::from(v1::ChatPermissionMode::Full));

    // A model without fast mode or the current effort falls back on both.
    let other = set(v1::ChatOptions {
        model: "fable-5-2".into(),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(
        (other.model.as_str(), other.effort.as_str()),
        ("fable-5-2", "high")
    );
    assert_eq!(other.speed, i32::from(v1::ChatSpeed::Standard));
    assert_eq!(other.permission, i32::from(v1::ChatPermissionMode::Full));
    // A model without efforts has none; back on one with them, the highest.
    let none = set(v1::ChatOptions {
        model: "sonnet-5-5".into(),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(none.effort, "");
    let other = set(v1::ChatOptions {
        model: "fable-5-2".into(),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(other.effort, "high");

    // What is asked for outright must exist, for the model it is asked with.
    // A chat whose provider cannot change a thing refuses a change to it, and
    // one whose model the catalog does not list keeps its effort and speed.
    let (quiet, _connection, sink) = quiet_runtime(&directory);
    let permission = SessionCommand::SetOptions(v1::ChatOptions {
        permission: v1::ChatPermissionMode::Full.into(),
        ..Default::default()
    });
    let refused = quiet.command(&pane("%1"), "quiet", permission.clone());
    assert_eq!(refused.unwrap_err().code, "chat_invalid_request");
    let unlisted = v1::ChatOptions {
        model: "unlisted".into(),
        effort: "high".into(),
        speed: v1::ChatSpeed::Fast.into(),
        permission: v1::ChatPermissionMode::Auto.into(),
    };
    sink.publish(Change::Catalog(v1::ChatCatalog {
        capabilities: Some(v1::ChatCapabilities {
            permission: true,
            effort: true,
            ..Default::default()
        }),
        ..Default::default()
    }));
    sink.publish(Change::State(v1::ChatState {
        options: Some(unlisted.clone()),
        ..Default::default()
    }));
    quiet.command(&pane("%1"), "quiet", permission).unwrap();
    let effort = SessionCommand::SetOptions(v1::ChatOptions {
        effort: "low".into(),
        ..Default::default()
    });
    let refused = quiet.command(&pane("%1"), "quiet", effort);
    assert_eq!(refused.unwrap_err().code, "chat_invalid_request");

    for refused in [
        v1::ChatOptions {
            model: "nope".into(),
            ..Default::default()
        },
        v1::ChatOptions {
            effort: "max".into(),
            ..Default::default()
        },
        v1::ChatOptions {
            speed: v1::ChatSpeed::Fast.into(),
            ..Default::default()
        },
        v1::ChatOptions {
            permission: 7,
            ..Default::default()
        },
        v1::ChatOptions {
            speed: 9,
            ..Default::default()
        },
    ] {
        let error = command(&runtime, "%1", SessionCommand::SetOptions(refused)).unwrap_err();
        assert_eq!(error.code, "chat_invalid_request");
    }
}

// --- subscriptions ----------------------------------------------------------

#[tokio::test]
async fn events_reach_only_the_connections_that_opened_the_pane_until_they_close_or_go() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = scripted_runtime(&directory);
    let mut opened = Connection::new();
    let mut elsewhere = Connection::new();
    let mut never = Connection::new();
    opened.open(&runtime, "%1");
    elsewhere.open(&runtime, "%2");

    send(&runtime, "%1", "go", "").unwrap();
    opened.until(turn_is(v1::ChatTurnState::Waiting)).await;
    assert!(elsewhere.is_quiet() && never.is_quiet());

    // CLOSE ends one pane's subscription; a departed connection loses all.
    elsewhere.open(&runtime, "%1");
    runtime.close(opened.id, &pane("%1"));
    command(&runtime, "%1", SessionCommand::Interrupt).unwrap();
    elsewhere.until(turn_is(v1::ChatTurnState::Idle)).await;
    assert!(opened.is_quiet());

    runtime.disconnect(elsewhere.id);
    send(&runtime, "%1", "again", "").unwrap();
    send(&runtime, "%2", "again", "").unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(elsewhere.is_quiet() && opened.is_quiet() && never.is_quiet());
    let panes = runtime.panes();
    assert!(panes.0.values().all(|pane| pane.subscribers.is_empty()));
}

#[tokio::test]
async fn a_snapshot_that_cannot_be_queued_subscribes_nobody() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = scripted_runtime(&directory);
    let mut connection = Connection::with_queue(1);
    let filler = SequencerControl::OrderedEvent(v1::HostEvent::default());
    connection.sender.try_send(filler).unwrap();
    let refused = runtime.open(connection.id, &pane("%1"), scripted::ADAPTER_ID);
    assert_eq!(refused.unwrap_err().code, "chat_queue_full");
    assert!(connection.is_quiet());
    send(&runtime, "%1", "go", "").unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(connection.is_quiet());
}

#[tokio::test]
async fn a_chat_opened_mid_turn_gets_the_snapshot_and_then_only_what_follows() {
    let directory = tempfile::tempdir().unwrap();
    let (runtime, mut first, sink) = quiet_runtime(&directory);
    let mut streaming = item("a1", v1::ChatItemKind::Assistant, "one");
    streaming.streaming = true;
    sink.publish(Change::Item(streaming));
    let delta = |text: &str| {
        sink.publish(Change::Delta(v1::ChatItemDelta {
            item_id: "a1".into(),
            text_append: text.into(),
        }));
    };
    delta(" two");

    let mut second = Connection::new();
    let snapshot = second.open(&runtime, "%1");
    assert_eq!(snapshot.revision, 2);
    assert_eq!(snapshot.items[0].text, "one two");
    delta(" three");
    let events = second.until(|_| true).await;
    assert_eq!(events[0].revision, 3);
    assert_eq!(
        first
            .until(|change| matches!(change, Change::Delta(delta) if delta.text_append == " three"))
            .await
            .len(),
        3
    );

    // Opening again brings a fresh snapshot and does not subscribe twice.
    assert_eq!(second.open(&runtime, "%1").items[0].text, "one two three");
    delta(" four");
    assert_eq!(second.until(|_| true).await[0].revision, 4);
    assert!(second.is_quiet());
}

#[tokio::test]
async fn a_subscriber_whose_queue_is_full_is_told_to_resync_and_stays_subscribed() {
    let directory = tempfile::tempdir().unwrap();
    let (runtime, _first, sink) = quiet_runtime(&directory);
    let mut slow = Connection::with_queue(8);
    runtime.open(slow.id, &pane("%1"), "quiet").unwrap();
    for index in 0..16 {
        let id = format!("i{index}");
        sink.publish(Change::Item(item(&id, v1::ChatItemKind::Assistant, "")));
    }
    // The queue held the snapshot and a few items; the rest were dropped,
    // which the connection learns as the gap every overflow costs.
    let resync = async {
        loop {
            match slow.events.recv().await.unwrap() {
                SequencerControl::InjectGap(event) => return event,
                _ => continue,
            }
        }
    };
    let resync = timeout(WAIT, resync).await.expect("no resync arrived");
    assert_eq!(resync.kind, i32::from(v1::EventKind::ResyncRequired));
    let panes = runtime.panes();
    assert!(panes.0[&pane("%1")].subscribers.contains(&slow.id));
}

#[tokio::test]
async fn a_closed_pane_is_forgotten_unless_another_connection_has_it_open() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = scripted_runtime(&directory);
    let (mut here, mut elsewhere) = (Connection::new(), Connection::new());
    here.open(&runtime, "%1");
    here.open(&runtime, "%2");
    elsewhere.open(&runtime, "%2");
    send(&runtime, "%3", "nobody is watching", "").unwrap();
    let held = |runtime: &ChatRuntime| -> Vec<String> {
        let panes = runtime.panes();
        panes.0.keys().map(|key| key.pane_id.clone()).collect()
    };
    assert_eq!(held(&runtime), ["%1", "%2", "%3"]);

    // This connection's server still has every pane: nothing goes.
    runtime.forget_closed(Some(here.id), "chat-tests", |_| true);
    assert_eq!(held(&runtime), ["%1", "%2", "%3"]);
    // Only %2 is left on it. %1 goes; %3 is not this connection's to judge,
    // since a pane newer than its view would look just as absent.
    runtime.forget_closed(Some(here.id), "chat-tests", |pane_id| pane_id == "%2");
    assert_eq!(held(&runtime), ["%2", "%3"]);
    // The host's own look at tmux can judge it, and leaves what is open.
    runtime.forget_closed(None, "chat-tests", |pane_id| pane_id == "%2");
    assert_eq!(held(&runtime), ["%2"]);
    runtime.forget_closed(None, "another-server", |_| true);
    assert_eq!(held(&runtime), ["%2"]);
    // The server was replaced, as this connection sees it: its panes are
    // another server's now. The other connection still has the chat open, so
    // it stays, but this one hears no more of it.
    runtime.forget_closed(Some(here.id), "another-server", |_| true);
    assert_eq!(held(&runtime), ["%2"]);
    send(&runtime, "%2", "go", "").unwrap();
    elsewhere.until(turn_is(v1::ChatTurnState::Waiting)).await;
    assert!(here.is_quiet());
    // Once the other connection sees the same, the chat is forgotten.
    runtime.forget_closed(Some(elsewhere.id), "another-server", |_| true);
    assert!(held(&runtime).is_empty());

    // The same pane id on the new server is a new chat.
    let reopened = PaneKey {
        server_identity: "another-server".into(),
        pane_id: "%1".into(),
    };
    runtime
        .open(here.id, &reopened, scripted::ADAPTER_ID)
        .unwrap();
    assert_eq!(runtime.panes().0[&reopened].items.len(), 233);
}

#[tokio::test]
async fn a_pane_without_a_registered_provider_has_no_chat() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = ChatRuntime::new(directory.path().join("settings.json"), Vec::new());
    assert_eq!(runtime.adapter_for(""), "");
    assert_eq!(runtime.adapter_for("claude"), "claude");
    for adapter_id in ["", "scripted", "claude"] {
        let refused = runtime.open(1, &pane("%1"), adapter_id);
        assert_eq!(refused.unwrap_err().code, "chat_unavailable");
    }
    // The scripted provider claims untagged panes only while it is registered.
    assert_eq!(scripted_runtime(&directory).adapter_for(""), "scripted");
    assert_eq!(scripted_runtime(&directory).adapter_for("claude"), "claude");
    assert!(scripted::enabled(Some("1".as_ref())));
    for off in [None, Some(""), Some("0"), Some("true")] {
        assert!(!scripted::enabled(off.map(str::as_ref)));
    }
}

// --- what the runtime itself enforces, with a provider that says nothing ----

struct Quiet;
struct QuietSession(ChatSink);

fn quiet_capabilities() -> v1::ChatCapabilities {
    v1::ChatCapabilities {
        rewind: true,
        ..Default::default()
    }
}

impl ChatProvider for Quiet {
    fn adapter_id(&self) -> &'static str {
        "quiet"
    }
    fn capabilities(&self) -> v1::ChatCapabilities {
        quiet_capabilities()
    }
    fn load_history(&self, _pane_id: &str) -> Vec<v1::ChatItem> {
        Vec::new()
    }
    fn connect(&self, _pane_id: &str, sink: ChatSink) -> Box<dyn ChatSession> {
        Box::new(QuietSession(sink))
    }
}

impl ChatSession for QuietSession {
    /// Does the least a session must: it rewinds, except the message "kept".
    fn command(&mut self, command: SessionCommand) -> Result<(), ChatError> {
        match command {
            SessionCommand::Rewind { item_id, .. } if item_id == "kept" => {
                Err(ChatError::new("chat_item_not_rewindable", "Not that one."))
            }
            SessionCommand::Rewind { after_item_id, .. } => {
                let truncate = v1::ChatTruncate { after_item_id };
                self.0.publish(Change::Truncate(truncate));
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// A runtime whose one pane, "%1", is driven by the returned sink.
fn quiet_runtime(directory: &tempfile::TempDir) -> (ChatRuntime, Connection, ChatSink) {
    let runtime = ChatRuntime::new(
        directory.path().join("settings.json"),
        vec![Arc::new(Quiet)],
    );
    let mut connection = Connection::new();
    connection.open(&runtime, "%1");
    let sink = ChatSink {
        panes: Arc::downgrade(&runtime.panes),
        key: pane("%1"),
    };
    (runtime, connection, sink)
}

fn item(id: &str, kind: v1::ChatItemKind, text: &str) -> v1::ChatItem {
    v1::ChatItem {
        id: id.into(),
        kind: kind.into(),
        text: text.into(),
        rewindable: kind == v1::ChatItemKind::User,
        ..Default::default()
    }
}

#[tokio::test]
async fn messages_are_checked_before_the_provider_sees_them() {
    let directory = tempfile::tempdir().unwrap();
    let (runtime, _connection, sink) = quiet_runtime(&directory);
    let send = |text: &str, item_id: &str| {
        let command = SessionCommand::Send {
            text: text.into(),
            item_id: item_id.into(),
        };
        runtime
            .command(&pane("%1"), "quiet", command)
            .map_err(|error| error.code)
    };
    sink.publish(Change::Item(item("m1", v1::ChatItemKind::User, "first")));

    assert_eq!(send("  \n", ""), Err("chat_invalid_request"));
    assert_eq!(send("again", "m1"), Err("chat_message_duplicate"));
    assert_eq!(
        send("x", &"i".repeat(MAX_ID_BYTES + 1)),
        Err("chat_invalid_request")
    );
    let long = "x".repeat(MAX_ITEM_TEXT_BYTES + 1);
    assert_eq!(send(&long, ""), Err("chat_message_too_large"));
    assert_eq!(send("fine", "m2"), Ok(()));

    // This provider cannot take a message mid-turn, so a turn in progress
    // refuses one, working or waiting.
    for turn in [v1::ChatTurnState::Working, v1::ChatTurnState::Waiting] {
        sink.publish(Change::State(v1::ChatState {
            turn: turn.into(),
            pending: Some(v1::ChatPendingRequest {
                request_id: "r1".into(),
                ..Default::default()
            }),
            ..Default::default()
        }));
        assert_eq!(send("now?", ""), Err("chat_turn_busy"));
    }

    // An answer is for the request that is pending, and has a size.
    let answer = |request_id: &str, other_text: String| {
        let answer = v1::ChatAnswer {
            answers: vec![v1::ChatQuestionAnswer {
                other_text,
                ..Default::default()
            }],
            ..Default::default()
        };
        let command = SessionCommand::Answer {
            request_id: request_id.into(),
            answer,
        };
        runtime
            .command(&pane("%1"), "quiet", command)
            .map_err(|error| error.code)
    };
    assert_eq!(answer("r0", String::new()), Err("chat_request_not_pending"));
    assert_eq!(
        answer("r1", "x".repeat(MAX_ANSWER_BYTES)),
        Err("chat_message_too_large")
    );
    assert_eq!(answer("r1", "fine".into()), Ok(()));
}

/// A provider whose `connect` does not return until it is told to.
struct Slow(Mutex<std::sync::mpsc::Receiver<()>>);

impl ChatProvider for Slow {
    fn adapter_id(&self) -> &'static str {
        "slow"
    }
    fn capabilities(&self) -> v1::ChatCapabilities {
        quiet_capabilities()
    }
    fn load_history(&self, _pane_id: &str) -> Vec<v1::ChatItem> {
        Vec::new()
    }
    fn connect(&self, _pane_id: &str, sink: ChatSink) -> Box<dyn ChatSession> {
        self.0.lock().unwrap().recv().unwrap();
        Box::new(QuietSession(sink))
    }
}

/// A provider whose chat is forgotten once while it connects, as a look at
/// tmux taken before the pane existed does to a chat nobody has open yet.
struct Forgotten(std::sync::atomic::AtomicBool);

impl ChatProvider for Forgotten {
    fn adapter_id(&self) -> &'static str {
        "forgotten"
    }
    fn capabilities(&self) -> v1::ChatCapabilities {
        quiet_capabilities()
    }
    fn load_history(&self, _pane_id: &str) -> Vec<v1::ChatItem> {
        vec![item("m1", v1::ChatItemKind::User, "kept on disk")]
    }
    fn connect(&self, _pane_id: &str, sink: ChatSink) -> Box<dyn ChatSession> {
        if self.0.swap(false, std::sync::atomic::Ordering::SeqCst) {
            let panes = sink.panes.upgrade().unwrap();
            let forgotten = panes.lock().unwrap().0.remove(&sink.key);
            assert!(forgotten.is_some());
        }
        Box::new(QuietSession(sink))
    }
}

#[test]
fn a_chat_forgotten_before_its_first_use_is_started_again() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Arc::new(Forgotten(false.into()));
    let shared: Arc<dyn ChatProvider> = provider.clone();
    let runtime = ChatRuntime::new(directory.path().join("settings.json"), vec![shared]);
    let forget_next = || {
        runtime.panes().0.clear();
        provider.0.store(true, std::sync::atomic::Ordering::SeqCst);
    };

    forget_next();
    let mut connection = Connection::new();
    assert_eq!(connection.open(&runtime, "%1").items.len(), 1);

    forget_next();
    let page = runtime.load_older(&pane("%1"), "forgotten", "m1").unwrap();
    assert!(page.older.is_empty());

    forget_next();
    let interrupt = SessionCommand::Interrupt;
    assert_eq!(runtime.command(&pane("%1"), "forgotten", interrupt), Ok(()));
    assert!(!provider.0.load(std::sync::atomic::Ordering::SeqCst));
}

/// A provider whose session keeps a message inside it until it is told to
/// go on, and only then starts the turn.
struct Gate(Mutex<Option<Gated>>);
struct Gated {
    sink: ChatSink,
    entered: std::sync::mpsc::Sender<()>,
    go_on: std::sync::mpsc::Receiver<()>,
}

impl ChatProvider for Gate {
    fn adapter_id(&self) -> &'static str {
        "gate"
    }
    fn capabilities(&self) -> v1::ChatCapabilities {
        quiet_capabilities()
    }
    fn load_history(&self, _pane_id: &str) -> Vec<v1::ChatItem> {
        Vec::new()
    }
    fn connect(&self, _pane_id: &str, sink: ChatSink) -> Box<dyn ChatSession> {
        let mut session = self.0.lock().unwrap().take().unwrap();
        session.sink = sink;
        Box::new(session)
    }
}

impl ChatSession for Gated {
    fn command(&mut self, _command: SessionCommand) -> Result<(), ChatError> {
        self.entered.send(()).unwrap();
        self.go_on.recv().unwrap();
        self.sink.publish(Change::State(v1::ChatState {
            turn: v1::ChatTurnState::Working.into(),
            ..Default::default()
        }));
        Ok(())
    }
}

#[test]
fn a_pane_takes_one_command_at_a_time_from_its_check_to_its_effect() {
    let directory = tempfile::tempdir().unwrap();
    let (entered, entries) = std::sync::mpsc::channel();
    let (release, go_on) = std::sync::mpsc::channel();
    let session = Gated {
        sink: ChatSink {
            panes: std::sync::Weak::new(),
            key: pane("%1"),
        },
        entered,
        go_on,
    };
    let runtime = ChatRuntime::new(
        directory.path().join("settings.json"),
        vec![Arc::new(Gate(Mutex::new(Some(session))))],
    );
    let send = |text: &str| {
        let command = SessionCommand::Send {
            text: text.into(),
            item_id: String::new(),
        };
        runtime
            .command(&pane("%1"), "gate", command)
            .map_err(|error| error.code)
    };
    std::thread::scope(|scope| {
        let first = scope.spawn(|| send("one"));
        entries.recv_timeout(WAIT).unwrap();
        // The first message is with the provider and the turn has not
        // started. A second, from another connection, is not checked against
        // the idle chat that is about to stop being idle: it waits.
        let second = scope.spawn(|| send("two"));
        std::thread::sleep(Duration::from_millis(100));
        release.send(()).unwrap();
        assert_eq!(first.join().unwrap(), Ok(()));
        assert_eq!(second.join().unwrap(), Err("chat_turn_busy"));
        assert!(entries.try_recv().is_err());
    });
}

#[test]
fn the_scripted_provider_is_registered_only_in_tests_and_by_its_switch() {
    let registered = |test_build, switch: Option<&str>| {
        let providers = registered_providers(test_build, switch.map(str::as_ref), Duration::ZERO);
        let ids: Vec<&str> = providers
            .iter()
            .map(|provider| provider.adapter_id())
            .collect();
        ids
    };
    assert!(registered(false, None).is_empty());
    assert!(registered(false, Some("0")).is_empty());
    assert_eq!(registered(false, Some("1")), ["scripted"]);
    assert_eq!(registered(true, None), ["scripted"]);
}

#[test]
fn a_command_for_a_pane_whose_session_is_still_connecting_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let (connected, wait) = std::sync::mpsc::channel();
    let runtime = ChatRuntime::new(
        directory.path().join("settings.json"),
        vec![Arc::new(Slow(Mutex::new(wait)))],
    );
    let interrupt = || runtime.command(&pane("%1"), "slow", SessionCommand::Interrupt);
    std::thread::scope(|scope| {
        let first = scope.spawn(interrupt);
        while !runtime.panes().0.contains_key(&pane("%1")) {
            std::thread::yield_now();
        }
        assert_eq!(interrupt().unwrap_err().code, "chat_not_ready");
        connected.send(()).unwrap();
        assert_eq!(first.join().unwrap(), Ok(()));
    });
    assert_eq!(interrupt(), Ok(()));
}

#[tokio::test]
async fn what_a_provider_publishes_is_bounded_and_numbered() {
    let directory = tempfile::tempdir().unwrap();
    let (runtime, mut connection, sink) = quiet_runtime(&directory);

    // A tool's output is cut to the preview bound and marked.
    let mut tool = item("t1", v1::ChatItemKind::Tool, "");
    tool.tool = Some(v1::ChatTool {
        output_preview: "é".repeat(MAX_OUTPUT_PREVIEW_BYTES),
        ..Default::default()
    });
    sink.publish(Change::Item(tool.clone()));
    // The same item again, and a delta for an item that is not streaming,
    // change nothing and take no revision.
    sink.publish(Change::Item(tool));
    sink.publish(Change::Delta(v1::ChatItemDelta {
        item_id: "t1".into(),
        text_append: "late".into(),
    }));

    // Streamed text stops at the item bound, on a character boundary.
    let mut streaming = item("a1", v1::ChatItemKind::Assistant, "");
    streaming.streaming = true;
    sink.publish(Change::Item(streaming));
    for _ in 0..3 {
        sink.publish(Change::Delta(v1::ChatItemDelta {
            item_id: "a1".into(),
            text_append: "é".repeat(MAX_ITEM_TEXT_BYTES / 4 + 1),
        }));
    }
    // The provider cannot rename the pane.
    sink.publish(Change::State(v1::ChatState {
        pane_id: "%9".into(),
        adapter_id: "other".into(),
        turn: v1::ChatTurnState::Working.into(),
        history_complete: true,
        ..Default::default()
    }));

    let events = connection.until(turn_is(v1::ChatTurnState::Working)).await;
    assert_consecutive(0, &events);
    assert_eq!(
        outline(&events),
        [
            ":unspecified",
            "assistant:streaming",
            "delta",
            "delta",
            "WORKING"
        ]
    );
    let Some(Change::Item(tool)) = &events[0].change else {
        unreachable!();
    };
    let preview = &tool.tool.as_ref().unwrap();
    assert_eq!(preview.output_preview.len(), MAX_OUTPUT_PREVIEW_BYTES);
    assert!(preview.output_truncated);
    let Some(Change::State(state)) = &events[4].change else {
        unreachable!();
    };
    assert_eq!(
        (state.pane_id.as_str(), state.adapter_id.as_str()),
        ("%1", "quiet")
    );

    let snapshot = connection.open(&runtime, "%1");
    assert_eq!(snapshot.revision, 5);
    let streamed = snapshot.items[1].text.len();
    assert!(streamed <= MAX_ITEM_TEXT_BYTES && streamed > MAX_ITEM_TEXT_BYTES - 2);
    let appended: usize = events
        .iter()
        .filter_map(|event| match &event.change {
            Some(Change::Delta(delta)) => Some(delta.text_append.len()),
            _ => None,
        })
        .sum();
    assert_eq!(appended, streamed);
}

#[tokio::test]
async fn an_item_published_again_is_an_update_in_place_and_keeps_its_kind() {
    let directory = tempfile::tempdir().unwrap();
    let (runtime, mut connection, sink) = quiet_runtime(&directory);
    for (id, kind) in [
        ("kept", v1::ChatItemKind::User),
        ("m2", v1::ChatItemKind::User),
        ("a1", v1::ChatItemKind::Assistant),
    ] {
        sink.publish(Change::Item(item(id, kind, "first")));
    }
    // An update changes the item where it stands, and so does an item
    // published as new under an id the timeline holds.
    sink.publish(Change::Updated(item(
        "m2",
        v1::ChatItemKind::User,
        "second",
    )));
    sink.publish(Change::Item(item(
        "a1",
        v1::ChatItemKind::Assistant,
        "second",
    )));
    // An update to an item the timeline does not hold is nothing: it was
    // rewound or trimmed away, and must not come back as the newest item.
    sink.publish(Change::Updated(item("gone", v1::ChatItemKind::Notice, "")));
    // Another kind under a held id is another item, and is not let in: the
    // first may be a message whose id a client chose.
    sink.publish(Change::Item(item("kept", v1::ChatItemKind::Reasoning, "")));
    sink.publish(Change::Item(item("new", v1::ChatItemKind::Notice, "")));
    let events = connection
        .until(|change| matches!(change, Change::Item(item) if item.id == "new"))
        .await;
    assert_consecutive(0, &events);
    let updated: Vec<&str> = events
        .iter()
        .filter_map(|event| match &event.change {
            Some(Change::Updated(item)) => Some(item.id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(updated, ["m2", "a1"]);
    assert_eq!(events.len(), 6);
    let ids = |items: &[v1::ChatItem]| -> Vec<String> {
        items.iter().map(|item| item.id.clone()).collect()
    };
    let snapshot = connection.open(&runtime, "%1");
    assert_eq!(ids(&snapshot.items), ["kept", "m2", "a1", "new"]);
    assert_eq!(snapshot.items[0].kind, i32::from(v1::ChatItemKind::User));
    assert_eq!(snapshot.items[1].text, "second");

    // A rewind the provider refuses drops nothing and says nothing.
    let rewind = |item_id: &str| runtime.command(&pane("%1"), "quiet", rewinding(item_id));
    assert_eq!(rewind("kept").unwrap_err().code, "chat_item_not_rewindable");
    assert!(connection.is_quiet());
    // A catalog that says nothing of the capabilities keeps them, rewind
    // among them.
    sink.publish(Change::Catalog(v1::ChatCatalog {
        models: vec![v1::ChatModel::default()],
        ..Default::default()
    }));
    let events = connection.until(|_| true).await;
    let Some(Change::Catalog(catalog)) = &events[0].change else {
        panic!("expected the catalog");
    };
    assert_eq!(catalog.capabilities, Some(quiet_capabilities()));
    rewind("m2").unwrap();
    let events = connection.until(|_| true).await;
    let truncate = v1::ChatTruncate {
        after_item_id: "kept".into(),
    };
    assert_eq!(events[0].change, Some(Change::Truncate(truncate)));
    assert_eq!(ids(&connection.open(&runtime, "%1").items), ["kept"]);

    // A request is pending only while the turn waits on it.
    let state = |turn: v1::ChatTurnState| {
        sink.publish(Change::State(v1::ChatState {
            turn: turn.into(),
            pending: Some(v1::ChatPendingRequest {
                request_id: "r1".into(),
                ..Default::default()
            }),
            ..Default::default()
        }));
    };
    state(v1::ChatTurnState::Working);
    state(v1::ChatTurnState::Waiting);
    let events = connection.until(turn_is(v1::ChatTurnState::Waiting)).await;
    let pending: Vec<bool> = events
        .iter()
        .map(|event| matches!(&event.change, Some(Change::State(state)) if state.pending.is_some()))
        .collect();
    assert_eq!(pending, [false, true]);
}

#[tokio::test]
async fn every_string_and_list_a_provider_publishes_is_cut_to_its_bound() {
    let directory = tempfile::tempdir().unwrap();
    let (runtime, mut connection, sink) = quiet_runtime(&directory);
    // Just past the bound of a label, and of a detail, with a character
    // across each boundary.
    let long = "é".repeat(MAX_LABEL_BYTES / 2 + 1);
    let longer = "é".repeat(MAX_DETAIL_BYTES / 2 + 1);

    // An item whose id cannot be kept whole is not an item.
    for id in [String::new(), "i".repeat(MAX_ID_BYTES + 1)] {
        sink.publish(Change::Item(item(&id, v1::ChatItemKind::Notice, "dropped")));
    }
    let mut tool = item("t1", v1::ChatItemKind::Tool, "");
    tool.tool = Some(v1::ChatTool {
        title: long.clone(),
        detail: longer.clone(),
        result_summary: long.clone(),
        path: longer.clone(),
        ..Default::default()
    });
    sink.publish(Change::Item(tool));
    let option = v1::ChatOption {
        id: long.clone(),
        label: long.clone(),
        description: long.clone(),
    };
    let question = v1::ChatQuestion {
        id: long.clone(),
        header: long.clone(),
        prompt: long.clone(),
        options: vec![option; MAX_QUESTION_OPTIONS + 1],
        ..Default::default()
    };
    sink.publish(Change::State(v1::ChatState {
        turn: v1::ChatTurnState::Waiting.into(),
        native_session_id: long.clone(),
        failure: long.clone(),
        options: Some(v1::ChatOptions {
            model: long.clone(),
            effort: long.clone(),
            ..Default::default()
        }),
        pending: Some(v1::ChatPendingRequest {
            request_id: long.clone(),
            title: long.clone(),
            detail: longer.clone(),
            questions: vec![question; MAX_QUESTIONS + 1],
            ..Default::default()
        }),
        ..Default::default()
    }));
    let model = v1::ChatModel {
        id: long.clone(),
        label: long.clone(),
        efforts: vec![long.clone(); MAX_MODEL_EFFORTS + 1],
        supports_fast: true,
    };
    let command = v1::ChatCommand {
        name: long.clone(),
        description: long.clone(),
        argument_hint: long.clone(),
        ..Default::default()
    };
    sink.publish(Change::Catalog(v1::ChatCatalog {
        capabilities: Some(quiet_capabilities()),
        models: vec![model; MAX_MODELS + 1],
        commands: vec![command; MAX_COMMANDS + 1],
    }));

    let events = connection
        .until(|change| matches!(change, Change::Catalog(_)))
        .await;
    assert_eq!(events.len(), 3);
    let snapshot = connection.open(&runtime, "%1");
    // What was sent is what is kept.
    assert_eq!(
        events[0].change,
        Some(Change::Item(snapshot.items[0].clone()))
    );
    assert_eq!(events[1].change, snapshot.state.clone().map(Change::State));
    assert_eq!(
        events[2].change,
        snapshot.catalog.clone().map(Change::Catalog)
    );

    let label = |text: &String| text.len() == MAX_LABEL_BYTES;
    let detail = |text: &String| text.len() == MAX_DETAIL_BYTES;
    let tool = snapshot.items[0].tool.as_ref().unwrap();
    assert!(label(&tool.title) && label(&tool.result_summary) && detail(&tool.detail));
    assert_eq!(tool.path.len(), MAX_PATH_BYTES);

    let state = snapshot.state.unwrap();
    let options = state.options.unwrap();
    assert!(label(&state.native_session_id) && label(&state.failure));
    assert!(label(&options.model) && label(&options.effort));
    let pending = state.pending.unwrap();
    assert!(label(&pending.request_id) && label(&pending.title) && detail(&pending.detail));
    assert_eq!(pending.questions.len(), MAX_QUESTIONS);
    for question in &pending.questions {
        assert!(label(&question.id) && label(&question.header) && label(&question.prompt));
        assert_eq!(question.options.len(), MAX_QUESTION_OPTIONS);
        for option in &question.options {
            assert!(label(&option.id) && label(&option.label) && label(&option.description));
        }
    }

    let catalog = snapshot.catalog.unwrap();
    assert_eq!(
        (catalog.models.len(), catalog.commands.len()),
        (MAX_MODELS, MAX_COMMANDS)
    );
    for model in &catalog.models {
        assert!(label(&model.id) && label(&model.label));
        assert_eq!(model.efforts.len(), MAX_MODEL_EFFORTS);
        assert!(model.efforts.iter().all(label));
    }
    for command in &catalog.commands {
        assert!(label(&command.name) && label(&command.description));
        assert!(label(&command.argument_hint));
    }
}

#[tokio::test]
async fn older_pages_walk_back_to_the_start_within_the_item_and_byte_bounds() {
    let directory = tempfile::tempdir().unwrap();
    let (runtime, mut connection, sink) = quiet_runtime(&directory);
    // 450 small items, then four so heavy that a page cannot hold them all.
    let mut published = Vec::new();
    for index in 0..450 {
        published.push(item(
            &format!("small{index}"),
            v1::ChatItemKind::Assistant,
            "small",
        ));
    }
    for index in 0..4 {
        let text = "x".repeat(MAX_ITEM_TEXT_BYTES);
        let mut heavy = item(&format!("heavy{index}"), v1::ChatItemKind::Tool, &text);
        heavy.tool = Some(v1::ChatTool {
            output_preview: "y".repeat(MAX_OUTPUT_PREVIEW_BYTES),
            ..Default::default()
        });
        published.push(heavy);
    }
    for item in &published {
        sink.publish(Change::Item(item.clone()));
    }

    let weight =
        |items: &[v1::ChatItem]| -> usize { items.iter().map(prost::Message::encoded_len).sum() };
    let snapshot = connection.open(&runtime, "%1");
    assert!(weight(&published[450..]) > MAX_PAGE_BYTES);
    assert!(weight(&snapshot.items) <= MAX_PAGE_BYTES);
    assert_eq!(snapshot.items, published[451..]);

    let mut pages = vec![snapshot.items];
    let mut cursor = snapshot.older_cursor;
    while !cursor.is_empty() {
        let page = runtime.load_older(&pane("%1"), "quiet", &cursor).unwrap();
        assert!(!page.older.is_empty() && page.older.len() <= MAX_PAGE_ITEMS);
        assert!(weight(&page.older) <= MAX_PAGE_BYTES);
        // A page says which revision it was read at.
        assert_eq!(page.revision, published.len() as u64);
        pages.insert(0, page.older);
        cursor = page.older_cursor;
    }
    let sizes: Vec<usize> = pages.iter().map(Vec::len).collect();
    assert_eq!(sizes, [51, 200, 200, 3]);
    assert_eq!(pages.concat(), published);

    for stale in ["", "gone"] {
        let refused = runtime.load_older(&pane("%1"), "quiet", stale);
        assert_eq!(refused.unwrap_err().code, "chat_cursor_invalid");
    }
}

#[tokio::test]
async fn a_timeline_past_its_bound_drops_the_oldest_and_says_history_is_incomplete() {
    let directory = tempfile::tempdir().unwrap();
    let (runtime, mut connection, sink) = quiet_runtime(&directory);
    for index in 0..=MAX_TIMELINE_ITEMS {
        let id = format!("i{index}");
        sink.publish(Change::Item(item(&id, v1::ChatItemKind::Assistant, "")));
    }
    let events = connection
        .until(|change| matches!(change, Change::State(_)))
        .await;
    assert_eq!(events.len(), MAX_TIMELINE_ITEMS + 2);
    assert_consecutive(0, &events);
    // A provider that still claims a complete history is corrected.
    sink.publish(Change::State(v1::ChatState {
        history_complete: true,
        turn: v1::ChatTurnState::Working.into(),
        ..Default::default()
    }));
    {
        let panes = runtime.panes();
        let held = &panes.0[&pane("%1")];
        assert_eq!(held.items.len(), MAX_TIMELINE_ITEMS);
        assert_eq!(held.items[0].id, "i1");
        assert!(!held.state.history_complete);
    }

    // The timeline's text is bounded too, whatever the number of items.
    let heavy = "x".repeat(MAX_ITEM_TEXT_BYTES);
    let fit = MAX_TIMELINE_BYTES / MAX_ITEM_TEXT_BYTES;
    for index in 0..fit + 2 {
        let id = format!("heavy{index}");
        sink.publish(Change::Item(item(&id, v1::ChatItemKind::Assistant, &heavy)));
    }
    let panes = runtime.panes();
    let held = &panes.0[&pane("%1")];
    assert_eq!(held.items.len(), fit);
    assert_eq!(held.items[0].id, "heavy2");
}

// --- the host's agent mode --------------------------------------------------

#[test]
fn the_agent_mode_defaults_to_terminal_and_survives_a_restart() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = scripted_runtime(&directory);
    assert_eq!(runtime.agent_mode(), v1::ChatAgentMode::Terminal);
    let refused = runtime.set_agent_mode(v1::ChatAgentMode::Unspecified);
    assert_eq!(refused.unwrap_err().code, "chat_invalid_request");

    runtime.set_agent_mode(v1::ChatAgentMode::Chat).unwrap();
    assert_eq!(runtime.agent_mode(), v1::ChatAgentMode::Chat);
    assert_eq!(
        scripted_runtime(&directory).agent_mode(),
        v1::ChatAgentMode::Chat
    );
    runtime.set_agent_mode(v1::ChatAgentMode::Terminal).unwrap();
    assert_eq!(
        scripted_runtime(&directory).agent_mode(),
        v1::ChatAgentMode::Terminal
    );

    // A settings file nobody can read is the default, and a mode that cannot
    // be written is not the mode.
    std::fs::write(directory.path().join("settings.json"), b"{not json").unwrap();
    assert_eq!(
        scripted_runtime(&directory).agent_mode(),
        v1::ChatAgentMode::Terminal
    );
    let nowhere = ChatRuntime::new(directory.path().join("settings.json/inside"), Vec::new());
    let refused = nowhere.set_agent_mode(v1::ChatAgentMode::Chat);
    assert_eq!(refused.unwrap_err().code, "chat_mode_not_saved");
    assert_eq!(nowhere.agent_mode(), v1::ChatAgentMode::Terminal);
}

// --- over the wire, against the process-wide runtime ------------------------

struct Client {
    stream: UnixStream,
    server: tokio::task::JoinHandle<anyhow::Result<()>>,
    next_request_id: u64,
}

impl Client {
    async fn connect() -> Self {
        let (mut stream, server) = UnixStream::pair().unwrap();
        let server = tokio::spawn(crate::service::serve_with_shutdown(server, None));
        let hello = envelope(1, 0, Payload::ClientHello(v1::ClientHello::default()));
        write_frame(&mut stream, &hello).await.unwrap();
        let hello = timeout(WAIT, read_frame(&mut stream)).await.unwrap();
        let hello = hello.unwrap().unwrap();
        assert_eq!(hello.protocol_major, tmux_agent_protocol::PROTOCOL_MAJOR);
        assert!(matches!(hello.payload, Some(Payload::ServerHello(_))));
        Self {
            stream,
            server,
            next_request_id: 1,
        }
    }

    async fn send(&mut self, request: v1::Request) -> u64 {
        self.next_request_id += 1;
        let frame = envelope(self.next_request_id, 0, Payload::Request(request));
        write_frame(&mut self.stream, &frame).await.unwrap();
        self.next_request_id
    }

    async fn chat(&mut self, chat: v1::ChatRequest) -> u64 {
        self.send(v1::Request {
            operation: v1::Operation::Chat.into(),
            chat: Some(chat),
            ..Default::default()
        })
        .await
    }

    /// The next frame's payload. An answer must be the one to `request_id`.
    async fn frame(&mut self, request_id: u64) -> Payload {
        let frame = timeout(WAIT, read_frame(&mut self.stream)).await;
        let frame = frame
            .expect("timed out waiting for a frame")
            .unwrap()
            .unwrap();
        let payload = frame.payload.unwrap();
        if matches!(payload, Payload::Response(_)) {
            assert_eq!(frame.request_id, request_id);
        }
        payload
    }

    /// Reads up to the answer to `request_id` and the chat event `last`
    /// accepts, whichever comes later. Returns the answer and every chat
    /// event read, in order. Other tests' broadcasts are skipped.
    async fn exchange(
        &mut self,
        request_id: u64,
        last: impl Fn(&Change) -> bool,
    ) -> (v1::Response, Vec<v1::ChatEvent>) {
        let (mut response, mut events, mut done) = (None, Vec::new(), false);
        while response.is_none() || !done {
            match self.frame(request_id).await {
                Payload::Response(answer) => response = Some(answer),
                Payload::Event(event) if event.kind == i32::from(v1::EventKind::Chat) => {
                    assert!(event.scope == event.chat.as_ref().unwrap().pane_id);
                    let event = event.chat.unwrap();
                    done |= last(event.change.as_ref().unwrap());
                    events.push(event);
                }
                Payload::Event(_) => {}
                other => panic!("unexpected frame {other:?}"),
            }
        }
        (response.unwrap(), events)
    }

    /// Opens a pane: its snapshot arrives as a chat event, and then the answer.
    async fn open(&mut self, pane_id: &str) -> v1::ChatSnapshot {
        let request_id = self.chat(chat(v1::ChatAction::Open, pane_id)).await;
        let snapshot = loop {
            match self.frame(request_id).await {
                Payload::Event(event) if event.kind == i32::from(v1::EventKind::Chat) => {
                    let event = event.chat.unwrap();
                    let Some(Change::Snapshot(snapshot)) = event.change else {
                        panic!("the first chat event after OPEN is not the snapshot");
                    };
                    assert_eq!(event.revision, snapshot.revision);
                    break snapshot;
                }
                Payload::Event(_) => {}
                other => panic!("OPEN was answered before its snapshot: {other:?}"),
            }
        };
        let response = self.answer(request_id).await;
        assert!(response.ok, "{}", response.display_message);
        snapshot
    }

    /// The answer to a request that causes no chat event.
    async fn answer(&mut self, request_id: u64) -> v1::Response {
        loop {
            match self.frame(request_id).await {
                Payload::Response(response) => return response,
                Payload::Event(event) => {
                    assert_ne!(event.kind, i32::from(v1::EventKind::Chat));
                }
                other => panic!("unexpected frame {other:?}"),
            }
        }
    }

    async fn close(self) {
        drop(self.stream);
        let _ = self.server.await;
    }
}

fn chat(action: v1::ChatAction, pane_id: &str) -> v1::ChatRequest {
    v1::ChatRequest {
        action: action.into(),
        pane_id: pane_id.into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn open_send_answer_rewind_and_load_older_over_the_wire() {
    const PANE: &str = "%7001";
    let mut client = Client::connect().await;
    // A second connection that never opens the pane.
    let mut bystander = Client::connect().await;

    let snapshot = client.open(PANE).await;
    assert_eq!(snapshot.items.len(), 200);
    assert_eq!(snapshot.state.as_ref().unwrap().adapter_id, "scripted");
    let last_before = snapshot.items.last().unwrap().id.clone();

    let request = client
        .chat(v1::ChatRequest {
            text: "Fix the wrapped paths".into(),
            client_message_id: "m1".into(),
            ..chat(v1::ChatAction::Send, PANE)
        })
        .await;
    let (response, events) = client
        .exchange(request, turn_is(v1::ChatTurnState::Waiting))
        .await;
    assert!(response.ok, "{}", response.display_message);
    assert_consecutive(snapshot.revision, &events);
    assert_eq!(
        outline(&events),
        [
            "user",
            "WORKING",
            "reasoning",
            "read:running",
            "read:ok",
            "run:running",
            "run:ok",
            "WORKING",
            "assistant:streaming",
            "delta",
            "delta",
            "delta",
            "assistant",
            "WAITING",
        ]
    );
    let request_id = pending(&events).request_id;
    let waiting = events.last().unwrap().revision;

    let request = client
        .chat(v1::ChatRequest {
            request_id,
            answer: Some(v1::ChatAnswer {
                answers: vec![v1::ChatQuestionAnswer {
                    question_id: "scope".into(),
                    option_ids: vec!["boxed".into()],
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..chat(v1::ChatAction::Answer, PANE)
        })
        .await;
    let (response, events) = client
        .exchange(request, turn_is(v1::ChatTurnState::Idle))
        .await;
    assert!(response.ok, "{}", response.display_message);
    assert_consecutive(waiting, &events);
    assert_eq!(
        outline(&events),
        [
            "WORKING",
            "edit:running",
            "edit:ok",
            "assistant",
            "WORKING",
            "IDLE"
        ]
    );
    let idle = events.last().unwrap().revision;

    let request = client
        .chat(v1::ChatRequest {
            item_id: "m1".into(),
            ..chat(v1::ChatAction::Rewind, PANE)
        })
        .await;
    let (response, events) = client
        .exchange(request, |change| matches!(change, Change::Truncate(_)))
        .await;
    assert!(response.ok, "{}", response.display_message);
    assert_consecutive(idle, &events);
    assert_eq!(outline(&events), ["truncate"]);
    let Some(Change::Truncate(truncate)) = &events[0].change else {
        unreachable!();
    };
    assert_eq!(truncate.after_item_id, last_before);

    let request = client
        .chat(v1::ChatRequest {
            older_cursor: snapshot.older_cursor.clone(),
            ..chat(v1::ChatAction::LoadOlder, PANE)
        })
        .await;
    let response = client.answer(request).await;
    assert!(response.ok, "{}", response.display_message);
    let page = response.chat.unwrap();
    assert_eq!(page.older.len(), 33);
    assert_eq!(page.revision, events[0].revision);
    assert_eq!(page.older[0].id, "history-q001");
    assert_eq!(page.older.last().unwrap().id, "history-q017");
    assert_eq!(snapshot.items[0].id, "history-a017");
    assert_eq!(page.older_cursor, "");

    // The bystander's own request is answered, and no chat event was queued
    // for it ahead of that answer or after it.
    let request = bystander
        .send(v1::Request {
            operation: v1::Operation::SessionHistory.into(),
            history: Some(v1::HistoryRequest {
                action: v1::HistoryAction::List.into(),
                ..Default::default()
            }),
            ..Default::default()
        })
        .await;
    let response = bystander.answer(request).await;
    assert!(response.ok && response.history.is_some());
    let request = bystander.chat(chat(v1::ChatAction::Close, PANE)).await;
    assert!(bystander.answer(request).await.ok);

    // A connection that goes away is unsubscribed.
    client.close().await;
    bystander.close().await;
    let key = PaneKey {
        server_identity: String::new(),
        pane_id: PANE.into(),
    };
    let panes = ChatRuntime::global().panes();
    let left = panes.0.get(&key);
    assert!(left.is_none_or(|pane| pane.subscribers.is_empty()));
}

#[tokio::test]
async fn refusals_name_their_reason_over_the_wire() {
    const PANE: &str = "%7002";
    let mut client = Client::connect().await;
    let history = |action: v1::HistoryAction| v1::Request {
        operation: v1::Operation::SessionHistory.into(),
        history: Some(v1::HistoryRequest {
            action: action.into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let operation = |operation: v1::Operation| v1::Request {
        operation: operation.into(),
        ..Default::default()
    };
    let chat_request = |chat: v1::ChatRequest| v1::Request {
        operation: v1::Operation::Chat.into(),
        chat: Some(chat),
        ..Default::default()
    };
    for (request, code) in [
        (operation(v1::Operation::Chat), "chat_invalid_request"),
        (
            chat_request(chat(v1::ChatAction::Unspecified, PANE)),
            "chat_invalid_request",
        ),
        (
            chat_request(chat(v1::ChatAction::Open, "7002; kill-server")),
            "chat_invalid_request",
        ),
        (
            chat_request(chat(v1::ChatAction::Open, "%99999999999")),
            "chat_invalid_request",
        ),
        (
            chat_request(v1::ChatRequest {
                expected_server_identity: "s".repeat(1025),
                ..chat(v1::ChatAction::Open, PANE)
            }),
            "chat_invalid_request",
        ),
        (
            chat_request(chat(v1::ChatAction::SetOptions, PANE)),
            "chat_invalid_request",
        ),
        (
            chat_request(chat(v1::ChatAction::Answer, PANE)),
            "chat_invalid_request",
        ),
        (
            chat_request(chat(v1::ChatAction::SetMode, "")),
            "chat_invalid_request",
        ),
        (
            chat_request(chat(v1::ChatAction::SideFork, PANE)),
            "chat_side_fork_unavailable",
        ),
        (
            chat_request(chat(v1::ChatAction::LoadOlder, PANE)),
            "chat_cursor_invalid",
        ),
        (
            operation(v1::Operation::SessionHistory),
            "history_invalid_request",
        ),
        (
            history(v1::HistoryAction::Unspecified),
            "history_invalid_request",
        ),
        // An action that names no recorded session.
        (
            history(v1::HistoryAction::Resume),
            "history_invalid_request",
        ),
        (
            history(v1::HistoryAction::Snooze),
            "history_invalid_request",
        ),
        (history(v1::HistoryAction::Wake), "history_invalid_request"),
    ] {
        let request_id = client.send(request).await;
        let response = client.answer(request_id).await;
        assert!(!response.ok);
        assert_eq!(response.error_code, code);
        assert!(!response.display_message.is_empty());
    }
    client.close().await;
}

#[tokio::test]
async fn set_mode_over_the_wire_stores_the_mode() {
    // Terminal, the default: this runtime is every other test's too, and
    // their agent snapshots carry its mode.
    let mut client = Client::connect().await;
    let request = client
        .chat(v1::ChatRequest {
            mode: v1::ChatAgentMode::Terminal.into(),
            ..chat(v1::ChatAction::SetMode, "")
        })
        .await;
    let response = client.answer(request).await;
    assert!(response.ok, "{}", response.display_message);
    assert_eq!(
        response.chat.unwrap().mode,
        i32::from(v1::ChatAgentMode::Terminal)
    );
    let runtime = ChatRuntime::global();
    assert_eq!(runtime.agent_mode(), v1::ChatAgentMode::Terminal);
    let stored = std::fs::read_to_string(&runtime.settings_path).unwrap();
    assert_eq!(stored, r#"{"agentMode":"terminal"}"#);
    client.close().await;
    // The test build's settings live in a directory of this process's own.
    let _ = std::fs::remove_dir_all(runtime.settings_path.parent().unwrap());
}
