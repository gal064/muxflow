//! `OPERATION_CHAT` (docs/design/chat-plan.md §4).
//!
//! Thin: the timeline, the subscriptions and every bound live in
//! `service::chat`; this maps a `ChatRequest` to a `ChatRuntime` call and its
//! outcome to a `Response`.

use std::sync::Mutex;

use tmux_agent_protocol::v1;

use super::super::SequencerControl;
use super::super::chat::{ChatError, ChatRuntime, PaneKey, SessionCommand};
use super::super::terminal::validate_tmux_id;
use super::{response_error, send_response};

/// "%" and the digits of a u32.
const MAX_PANE_ID_BYTES: usize = 11;
const MAX_SERVER_IDENTITY_BYTES: usize = 1024;

pub(super) async fn handle(
    request_id: u64,
    request: v1::Request,
    control_tx: &tokio::sync::mpsc::Sender<SequencerControl>,
    connection_id: u64,
    topology_baseline: &Mutex<Option<(tmux_control::TmuxSnapshot, String)>>,
) {
    let outcome = match request.chat {
        None => Err(invalid("A chat operation needs a chat payload.")),
        Some(chat) => dispatch(chat, connection_id, topology_baseline),
    };
    let response = match outcome {
        Ok(chat) => v1::Response {
            ok: true,
            chat: Some(chat),
            ..Default::default()
        },
        Err(error) => response_error(error.code, &error.message),
    };
    send_response(control_tx, request_id, response).await;
}

fn dispatch(
    request: v1::ChatRequest,
    connection_id: u64,
    topology_baseline: &Mutex<Option<(tmux_control::TmuxSnapshot, String)>>,
) -> Result<v1::ChatResponse, ChatError> {
    use v1::ChatAction as Action;
    let runtime = ChatRuntime::global();
    let action = Action::try_from(request.action).unwrap_or_default();
    match action {
        Action::Unspecified => return Err(invalid("Unknown chat action.")),
        Action::SetMode => {
            let mode = v1::ChatAgentMode::try_from(request.mode).unwrap_or_default();
            runtime.set_agent_mode(mode)?;
            return Ok(v1::ChatResponse {
                mode: mode.into(),
                ..Default::default()
            });
        }
        _ => {}
    }

    // Every other action names a pane of the tmux server the caller saw it on.
    // Both name the chat from here on, so neither is taken at any length.
    if validate_tmux_id(&request.pane_id, '%').is_err() || request.pane_id.len() > MAX_PANE_ID_BYTES
    {
        return Err(invalid("A chat action needs a pane."));
    }
    if request.expected_server_identity.len() > MAX_SERVER_IDENTITY_BYTES {
        return Err(invalid("That is not a tmux server."));
    }
    let key = PaneKey {
        server_identity: request.expected_server_identity,
        pane_id: request.pane_id,
    };
    let baseline = topology_baseline.lock().unwrap();
    let pane = |pane_id: &str| {
        let (snapshot, _) = baseline.as_ref()?;
        snapshot.panes.iter().find(|pane| pane.id == pane_id)
    };
    if let Some((_, identity)) = baseline.as_ref() {
        runtime.forget_closed(Some(connection_id), identity, |pane_id| {
            pane(pane_id).is_some()
        });
    }
    // Whatever else is wrong with the request, a connection can let go.
    if action == Action::Close {
        runtime.close(connection_id, &key);
        return Ok(v1::ChatResponse::default());
    }
    // The pane must be one of the server this connection last saw. A
    // connection with no topology has nothing to compare against and is taken
    // at its word, with its pane untagged: one that never asked for it, or
    // one that has just launched an agent and not yet heard the result.
    let tagged = match baseline.as_ref() {
        Some((_, identity)) if *identity != key.server_identity => {
            return Err(ChatError::new(
                "chat_server_changed",
                "The tmux server changed. Reopen the chat.",
            ));
        }
        Some(_) => pane(&key.pane_id)
            .ok_or_else(|| ChatError::new("chat_unavailable", "That pane has closed."))?
            .chat_adapter_id
            .clone(),
        None => String::new(),
    };
    drop(baseline);
    let adapter_id = runtime.adapter_for(&tagged);
    let command = |command| {
        runtime
            .command(&key, &adapter_id, command)
            .map(|()| v1::ChatResponse::default())
    };
    match action {
        Action::Open => {
            runtime.open(connection_id, &key, &adapter_id)?;
            Ok(v1::ChatResponse::default())
        }
        Action::Send => command(SessionCommand::Send {
            text: request.text,
            item_id: request.client_message_id,
        }),
        Action::Interrupt => command(SessionCommand::Interrupt),
        // A missing answer is not a denial.
        Action::Answer => command(SessionCommand::Answer {
            request_id: request.request_id,
            answer: request
                .answer
                .ok_or_else(|| invalid("Answering needs the answer."))?,
        }),
        Action::Rewind => command(SessionCommand::Rewind {
            item_id: request.item_id,
            // Filled in by the runtime, which holds the timeline.
            after_item_id: String::new(),
        }),
        Action::SetOptions => {
            command(SessionCommand::SetOptions(request.options.ok_or_else(
                || invalid("Changing options needs the options."),
            )?))
        }
        Action::LoadOlder => runtime.load_older(&key, &adapter_id, &request.older_cursor),
        // Stream B wires this to the agent branch action.
        Action::SideFork => Err(ChatError::new(
            "chat_side_fork_unavailable",
            "Side questions are not available on this host yet.",
        )),
        Action::Unspecified | Action::SetMode | Action::Close => {
            unreachable!("answered above")
        }
    }
}

fn invalid(message: &str) -> ChatError {
    ChatError::new("chat_invalid_request", message)
}

#[cfg(test)]
mod tests {
    use tmux_agent_protocol::v1::{ChatAction as Action, chat_event::Change};

    use super::*;
    use crate::service::events::register_control_event_sink;

    type Baseline = Option<(tmux_control::TmuxSnapshot, String)>;

    fn baseline(server_identity: &str, pane_ids: &[&str]) -> Baseline {
        let pane = |id: &&str| tmux_control::Pane {
            id: (*id).to_owned(),
            session_id: "$1".into(),
            window_id: "@1".into(),
            index: 0,
            active: false,
            width: 80,
            height: 24,
            left: 0,
            top: 0,
            current_path: String::new(),
            current_command: String::new(),
            pane_pid: 0,
            start_command: String::new(),
            title: String::new(),
            chat_adapter_id: String::new(),
        };
        let snapshot = tmux_control::TmuxSnapshot {
            panes: pane_ids.iter().map(pane).collect(),
            ..Default::default()
        };
        Some((snapshot, server_identity.to_owned()))
    }

    fn request(action: Action, server_identity: &str, pane_id: &str) -> v1::ChatRequest {
        v1::ChatRequest {
            action: action.into(),
            pane_id: pane_id.into(),
            expected_server_identity: server_identity.into(),
            ..Default::default()
        }
    }

    /// The ids here are this test's own: the runtime is the process's.
    #[tokio::test]
    async fn a_pane_action_is_for_a_pane_of_the_server_the_connection_last_saw() {
        const OLD: &str = "dispatch-test-old-server";
        const NEW: &str = "dispatch-test-new-server";
        const PANE: &str = "%9101";
        let (sender, mut queue) = tokio::sync::mpsc::channel(64);
        let connection = register_control_event_sink(sender);
        let seen = Mutex::new(baseline(OLD, &[PANE]));
        let outcome = |request: v1::ChatRequest| {
            dispatch(request, connection.id, &seen)
                .map(|_| ())
                .map_err(|error| error.code)
        };
        let mut snapshot = || loop {
            let SequencerControl::OrderedEvent(event) = queue.try_recv().expect("no snapshot")
            else {
                continue;
            };
            if let Some(Change::Snapshot(snapshot)) = event.chat.and_then(|chat| chat.change) {
                return snapshot;
            }
        };
        let send = |server_identity: &str, pane_id: &str| v1::ChatRequest {
            text: "go".into(),
            client_message_id: "m1".into(),
            ..request(Action::Send, server_identity, pane_id)
        };

        assert_eq!(outcome(request(Action::Open, OLD, PANE)), Ok(()));
        assert_eq!(snapshot().items.len(), 200);
        assert_eq!(outcome(send(OLD, PANE)), Ok(()));
        // Another server's pane, and a pane this server does not have.
        assert_eq!(outcome(send(NEW, PANE)), Err("chat_server_changed"));
        assert_eq!(outcome(send(OLD, "%9102")), Err("chat_unavailable"));

        // The server is replaced. Its chat can no longer be acted on, but it
        // can always be let go.
        *seen.lock().unwrap() = baseline(NEW, &[PANE]);
        assert_eq!(
            outcome(request(Action::Interrupt, OLD, PANE)),
            Err("chat_server_changed")
        );
        assert_eq!(outcome(request(Action::Close, OLD, PANE)), Ok(()));
        // The same pane id on the new server is a new chat, without the
        // message sent to the old one.
        assert_eq!(outcome(request(Action::Open, NEW, PANE)), Ok(()));
        let reopened = snapshot();
        assert_eq!(reopened.items.len(), 200);
        assert!(reopened.items.iter().all(|item| item.id != "m1"));
        assert_eq!(outcome(send(NEW, PANE)), Ok(()));

        // Once the pane closes, its chat is forgotten with it.
        *seen.lock().unwrap() = baseline(NEW, &[]);
        assert_eq!(outcome(request(Action::Close, NEW, PANE)), Ok(()));
        assert_eq!(
            outcome(request(Action::Open, NEW, PANE)),
            Err("chat_unavailable")
        );
        *seen.lock().unwrap() = baseline(NEW, &[PANE]);
        assert_eq!(outcome(send(NEW, PANE)), Ok(()));
    }
}
