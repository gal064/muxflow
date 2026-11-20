//! `OPERATION_CHAT` (docs/design/chat-plan.md §4).
//!
//! Thin: the timeline, the subscriptions and every bound live in
//! `service::chat`; this maps a `ChatRequest` to a `ChatRuntime` call and its
//! outcome to a `Response`.

use std::sync::{Arc, Mutex};

use tmux_agent_protocol::v1;

use super::super::SequencerControl;
use super::super::chat::{
    Attached, ChatError, ChatRuntime, PaneKey, SessionCommand, admit_attachments, open_fork,
    terminal_commands as commands_of,
};
use super::super::snapshot::tmux_command;
use super::super::terminal::validate_tmux_id;
use super::super::tmux_actions::escaped_format_literal;
use super::agent_dispatch::agent_launch_argv;
use super::{response_error, send_response};

/// "%" and the digits of a u32.
const MAX_PANE_ID_BYTES: usize = 11;
const MAX_SERVER_IDENTITY_BYTES: usize = 1024;
/// The longest side question. It starts its fork on the agent's command
/// line, and tmux takes no command much longer than this.
const MAX_SIDE_QUESTION_BYTES: usize = 8 * 1024;

/// The topology a connection last saw, and the identity of its server.
type Baseline = Option<(tmux_control::TmuxSnapshot, String)>;

pub(super) async fn handle(
    request_id: u64,
    request: v1::Request,
    control_tx: &tokio::sync::mpsc::Sender<SequencerControl>,
    connection_id: u64,
    topology_baseline: &Arc<Mutex<Baseline>>,
) {
    let outcome = match request.chat {
        None => Err(invalid("A chat operation needs a chat payload.")),
        Some(mut chat) => {
            // The files a message names are checked, and its inline images
            // read, before anything else of it: off the runtime, and with no
            // lock held.
            let wanted = std::mem::take(&mut chat.attachments);
            let sends = chat.action == i32::from(v1::ChatAction::Send);
            let attached = match wanted.is_empty() || !sends {
                true => Ok(Vec::new()),
                false => tokio::task::spawn_blocking(move || admit_attachments(wanted))
                    .await
                    .unwrap_or_else(|_| {
                        Err(ChatError::new(
                            "chat_attachment_missing",
                            "The attachments could not be read.",
                        ))
                    }),
            };
            let blocks = [
                v1::ChatAction::Fork,
                v1::ChatAction::SideFork,
                v1::ChatAction::SetMode,
            ]
            .iter()
            .any(|action| chat.action == i32::from(*action));
            match attached {
                // A fork reads the agent's record, runs tmux and reads the
                // agent's process, and a change of mode is synced to disk:
                // off the runtime's workers, as an agent operation is, so a
                // stalled disk parks only the blocking pool.
                Ok(attached) if blocks => {
                    let baseline = Arc::clone(topology_baseline);
                    tokio::task::spawn_blocking(move || {
                        dispatch(chat, attached, connection_id, &baseline)
                    })
                    .await
                    .unwrap_or_else(|_| {
                        Err(ChatError::new(
                            "chat_request_failed",
                            "The request could not be run.",
                        ))
                    })
                }
                Ok(attached) => dispatch(chat, attached, connection_id, topology_baseline),
                Err(error) => Err(error),
            }
        }
    };
    respond(control_tx, request_id, outcome).await;
}

async fn respond(
    control_tx: &tokio::sync::mpsc::Sender<SequencerControl>,
    request_id: u64,
    outcome: Result<v1::ChatResponse, ChatError>,
) {
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

/// The request is TERMINAL_COMMANDS, which [`terminal_commands`] answers.
pub(super) fn lists_terminal_commands(request: &v1::Request) -> bool {
    request
        .chat
        .as_ref()
        .is_some_and(|chat| chat.action == i32::from(v1::ChatAction::TerminalCommands))
}

/// Answers TERMINAL_COMMANDS, on the blocking pool: the pane's agent may be
/// started to ask it.
pub(super) async fn terminal_commands(
    request_id: u64,
    request: v1::Request,
    control_tx: &tokio::sync::mpsc::Sender<SequencerControl>,
    topology_baseline: Arc<Mutex<Baseline>>,
) {
    let outcome = match request.chat {
        None => Err(invalid("A chat operation needs a chat payload.")),
        Some(chat) => {
            tokio::task::spawn_blocking(move || terminal_commands_of(&chat, &topology_baseline))
                .await
                .unwrap_or_else(|_| {
                    Err(ChatError::new(
                        "chat_request_failed",
                        "The request could not be run.",
                    ))
                })
        }
    };
    let outcome = outcome.map(|commands| v1::ChatResponse {
        commands,
        ..Default::default()
    });
    respond(control_tx, request_id, outcome).await;
}

/// The commands of the agent in the terminal pane `request` names, which is
/// checked as any pane action's is. Blocks for as long as asking the agent
/// takes.
fn terminal_commands_of(
    request: &v1::ChatRequest,
    topology_baseline: &Mutex<Baseline>,
) -> Result<Vec<v1::ChatCommand>, ChatError> {
    check_pane(request)?;
    let (pane_pid, current_path) = match topology_baseline.lock().unwrap().as_ref() {
        Some((_, identity)) if *identity != request.expected_server_identity => {
            return Err(server_changed());
        }
        Some((snapshot, _)) => {
            let pane = snapshot
                .panes
                .iter()
                .find(|pane| pane.id == request.pane_id)
                .ok_or_else(pane_closed)?;
            (pane.pane_pid, pane.current_path.clone())
        }
        // A connection with no topology has no pane to look in.
        None => return Ok(Vec::new()),
    };
    Ok(commands_of(pane_pid, &current_path))
}

/// Every action but SET_MODE names a pane of the tmux server the caller saw
/// it on. Both name the chat, so neither is taken at any length.
fn check_pane(request: &v1::ChatRequest) -> Result<(), ChatError> {
    if validate_tmux_id(&request.pane_id, '%').is_err() || request.pane_id.len() > MAX_PANE_ID_BYTES
    {
        return Err(invalid("A chat action needs a pane."));
    }
    if request.expected_server_identity.len() > MAX_SERVER_IDENTITY_BYTES {
        return Err(invalid("That is not a tmux server."));
    }
    Ok(())
}

fn server_changed() -> ChatError {
    ChatError::new(
        "chat_server_changed",
        "The tmux server changed. Reopen the chat.",
    )
}

fn pane_closed() -> ChatError {
    ChatError::new("chat_unavailable", "That pane has closed.")
}

fn dispatch(
    request: v1::ChatRequest,
    attachments: Vec<Attached>,
    connection_id: u64,
    topology_baseline: &Mutex<Baseline>,
) -> Result<v1::ChatResponse, ChatError> {
    use v1::ChatAction as Action;
    let runtime = ChatRuntime::global();
    let action = Action::try_from(request.action).unwrap_or_default();
    match action {
        Action::Unspecified => return Err(invalid("Unknown chat action.")),
        // Answered by `terminal_commands`, off the ordered lane.
        Action::TerminalCommands => return Err(invalid("That is not a chat action.")),
        Action::SetMode => {
            let mode = v1::ChatAgentMode::try_from(request.mode).unwrap_or_default();
            let mode = if request.initialize_if_unset {
                runtime.configure_agent_mode(mode, true)?
            } else {
                runtime.set_agent_mode(mode)?;
                mode
            };
            return Ok(v1::ChatResponse {
                mode: mode.into(),
                ..Default::default()
            });
        }
        _ => {}
    }

    check_pane(&request)?;
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
    let (tagged, title) = match baseline.as_ref() {
        Some((_, identity)) if *identity != key.server_identity => {
            return Err(server_changed());
        }
        Some((snapshot, _)) => {
            let pane = pane(&key.pane_id).ok_or_else(pane_closed)?;
            // A tab with a locked name publishes no pane title: that name is
            // the one its agent goes by.
            let title = if pane.title.is_empty() {
                let tab = |window: &&tmux_control::Window| window.id == pane.window_id;
                snapshot
                    .windows
                    .iter()
                    .find(tab)
                    .map_or("", |window| window.name.as_str())
            } else {
                pane.title.as_str()
            };
            (pane.chat_adapter_id.clone(), title.to_owned())
        }
        None => (String::new(), String::new()),
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
            attachments,
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
        // The runtime checks that the state lists it as one that can be stopped.
        Action::StopTask => command(SessionCommand::StopTask {
            task_id: request.task_id,
        }),
        Action::LoadOlder => runtime.load_older(&key, &adapter_id, &request.older_cursor),
        Action::SideFork => {
            let forked_pane_id = side_fork(runtime, &key, &adapter_id, &request.text, &title)?;
            // The topology actor publishes the server as it is with the fork.
            *topology_baseline.lock().unwrap() = None;
            Ok(v1::ChatResponse {
                forked_pane_id,
                ..Default::default()
            })
        }
        // A reply's fork: the chat starts it itself, beside its source.
        Action::Fork => {
            let forked_pane_id =
                runtime.fork(&key, &adapter_id, &request.item_id, &title, open_fork)?;
            *topology_baseline.lock().unwrap() = None;
            Ok(v1::ChatResponse {
                forked_pane_id,
                ..Default::default()
            })
        }
        Action::Unspecified | Action::SetMode | Action::Close | Action::TerminalCommands => {
            unreachable!("answered above")
        }
    }
}

/// Opens a copy of the pane's conversation in a split beside it, with
/// `text` as the copy's first message: the agent's own branch command, the
/// way the Agents list starts a branch, and the question after it. In Chat
/// mode the copy becomes a chat like any agent that starts.
fn side_fork(
    runtime: &ChatRuntime,
    key: &PaneKey,
    adapter_id: &str,
    text: &str,
    title: &str,
) -> Result<String, ChatError> {
    let unavailable = |message: &str| ChatError::new("chat_side_fork_unavailable", message);
    if text.trim().is_empty() {
        return Err(invalid("A side question needs some text."));
    }
    if text.len() > MAX_SIDE_QUESTION_BYTES {
        return Err(ChatError::new(
            "chat_message_too_large",
            "That side question is too long. Ask it in a shorter message.",
        ));
    }
    let source = runtime
        .conversation(key, adapter_id)
        .ok_or_else(|| unavailable("This chat has no conversation to fork yet."))?;
    let adapter = super::super::agents::adapters::by_id(&source.adapter_id)
        .ok_or_else(|| unavailable("This chat's agent cannot be forked."))?;
    let mut launch = adapter.branch(&source.native_session_id, title);
    // The fork has its source's model and permissions.
    launch.arguments.extend(source.arguments);
    launch.arguments.extend(["--".to_owned(), text.to_owned()]);
    let failed = |_| unavailable("The fork could not be opened.");
    let output = tmux_command()
        .map_err(failed)?
        .args(["split-window", "-h", "-P", "-F", "#{pane_id}", "-t"])
        .arg(&key.pane_id)
        .arg("-c")
        // tmux reads a start directory as a format.
        .arg(escaped_format_literal(&source.folder))
        .args(agent_launch_argv(launch))
        // Marked in the same step, on the pane the split made: the chat the
        // fork becomes reads the mark, and may be starting already.
        .args([
            ";",
            "set-option",
            "-p",
            super::super::chat::SIDE_OPTION,
            "1",
        ])
        .output()
        .map_err(|_| unavailable("The fork could not be opened."))?;
    let forked = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !output.status.success() || validate_tmux_id(&forked, '%').is_err() {
        return Err(unavailable("The fork could not be opened."));
    }
    // So that the fork is never listed as a session of its own.
    super::super::history::History::global().hide_pane(&forked);
    Ok(forked)
}

fn invalid(message: &str) -> ChatError {
    ChatError::new("chat_invalid_request", message)
}

#[cfg(test)]
mod tests {
    use tmux_agent_protocol::v1::{ChatAction as Action, chat_event::Change};

    use super::*;
    use crate::service::events::register_control_event_sink;

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
            dispatch(request, Vec::new(), connection.id, &seen)
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

    /// The refusals come before anything is opened: no pane is made here.
    #[tokio::test]
    async fn a_fork_of_a_reply_names_the_reply_and_is_refused_mid_turn() {
        const SERVER: &str = "dispatch-test-fork-server";
        const PANE: &str = "%9111";
        let (sender, _queue) = tokio::sync::mpsc::channel(4096);
        let connection = register_control_event_sink(sender);
        let seen = Mutex::new(baseline(SERVER, &[PANE]));
        let outcome = |request: v1::ChatRequest| {
            dispatch(request, Vec::new(), connection.id, &seen).map_err(|error| error.code)
        };
        let fork = |item_id: &str| v1::ChatRequest {
            item_id: item_id.into(),
            ..request(Action::Fork, SERVER, PANE)
        };
        assert!(outcome(request(Action::Open, SERVER, PANE)).is_ok());
        assert_eq!(
            outcome(fork("no-such-item")).unwrap_err(),
            "chat_item_not_found"
        );
        assert_eq!(outcome(fork("")).unwrap_err(), "chat_item_not_found");
        // The scripted chat starts a turn, whose reply is not over.
        let send = v1::ChatRequest {
            text: "go".into(),
            client_message_id: "m1".into(),
            ..request(Action::Send, SERVER, PANE)
        };
        assert!(outcome(send).is_ok());
        assert_eq!(outcome(fork("m1")).unwrap_err(), "chat_turn_busy");
        assert!(seen.lock().unwrap().is_some(), "nothing was opened");
    }

    /// A fork, which is run on the blocking pool, is answered as any other
    /// request is: through the connection's ordered lane.
    #[tokio::test]
    async fn a_fork_is_answered_on_the_ordered_lane() {
        const SERVER: &str = "dispatch-test-fork-lane-server";
        const PANE: &str = "%9121";
        let (sender, _queue) = tokio::sync::mpsc::channel(4096);
        let connection = register_control_event_sink(sender);
        let seen = Arc::new(Mutex::new(baseline(SERVER, &[PANE])));
        assert!(
            dispatch(
                request(Action::Open, SERVER, PANE),
                Vec::new(),
                connection.id,
                &seen
            )
            .is_ok()
        );
        let (control_tx, mut control_rx) = tokio::sync::mpsc::channel(4);
        for action in [Action::Fork, Action::SideFork] {
            let chat = v1::ChatRequest {
                item_id: "no-such-item".into(),
                ..request(action, SERVER, PANE)
            };
            let request = v1::Request {
                chat: Some(chat),
                ..Default::default()
            };
            handle(7, request, &control_tx, connection.id, &seen).await;
            let Some(SequencerControl::Response {
                request_id,
                response,
                ..
            }) = control_rx.recv().await
            else {
                panic!("no response");
            };
            assert_eq!(request_id, 7);
            assert!(!response.ok);
            // The reply is not in the chat; a side question needs its text.
            let expected = match action {
                Action::Fork => "chat_item_not_found",
                _ => "chat_invalid_request",
            };
            assert_eq!(response.error_code, expected);
        }
        assert!(seen.lock().unwrap().is_some(), "nothing was opened");
    }

    /// The pane is checked as a chat's is; one with no agent in it, here
    /// one with no process at all, lists nothing.
    #[tokio::test]
    async fn a_terminals_commands_are_for_a_pane_of_the_server_the_connection_saw() {
        const SERVER: &str = "dispatch-test-terminal-commands-server";
        const PANE: &str = "%9131";
        let seen = Arc::new(Mutex::new(baseline(SERVER, &[PANE])));
        let commands = |server_identity: &str, pane_id: &str| {
            terminal_commands_of(
                &request(Action::TerminalCommands, server_identity, pane_id),
                &seen,
            )
            .map_err(|error| error.code)
        };
        assert_eq!(commands(SERVER, PANE), Ok(Vec::new()));
        assert_eq!(commands("another-server", PANE), Err("chat_server_changed"));
        assert_eq!(commands(SERVER, "%9132"), Err("chat_unavailable"));
        assert_eq!(commands(SERVER, ""), Err("chat_invalid_request"));
        // Not a chat's action.
        assert_eq!(
            dispatch(
                request(Action::TerminalCommands, SERVER, PANE),
                Vec::new(),
                0,
                &seen
            )
            .map_err(|error| error.code),
            Err("chat_invalid_request")
        );

        let wanted = v1::Request {
            chat: Some(request(Action::TerminalCommands, SERVER, PANE)),
            ..Default::default()
        };
        assert!(lists_terminal_commands(&wanted));
        let (control_tx, mut control_rx) = tokio::sync::mpsc::channel(4);
        terminal_commands(9, wanted, &control_tx, Arc::clone(&seen)).await;
        let Some(SequencerControl::Response {
            request_id,
            response,
            ..
        }) = control_rx.recv().await
        else {
            panic!("no response");
        };
        assert_eq!(request_id, 9);
        assert!(response.ok);
        assert!(response.chat.unwrap().commands.is_empty());
    }
}
