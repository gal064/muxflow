//! Chat mode (docs/design/chat-plan.md §4): one request to a host's helper,
//! and the chat events it pushes for the panes this client opened.
//!
//! A chat's snapshot is one of those events, so everything about a pane
//! arrives in order on the event channel; the answer to `OPEN` says only that
//! the snapshot is on its way. `ChatEvent` in `chat.proto` says how a client
//! follows revisions, and that it sends `OPEN` again for every chat it has
//! open or has asked to open once a resync or a reconnect has finished: the
//! bridge drops the events that arrive while it resyncs, a snapshot among
//! them.
//!
//! The messages cross this crate as the protocol's own types rather than as
//! JSON: nothing but Rust reads them, and a second copy of some thirty
//! messages would only be somewhere for the two to drift apart.

use prost::Message as _;
use tmux_agent_protocol::v1;

pub use tmux_agent_protocol::v1::{
    ChatAction, ChatAgentMode, ChatAnswer, ChatCapabilities, ChatCatalog, ChatCommand,
    ChatCommandKind, ChatConnection, ChatEvent, ChatItem, ChatItemDelta, ChatItemKind, ChatModel,
    ChatOption, ChatOptions, ChatPendingRequest, ChatPermissionMode, ChatQuestion,
    ChatQuestionAnswer, ChatRequest, ChatRequestKind, ChatResponse, ChatSnapshot, ChatSpeed,
    ChatState, ChatTool, ChatToolKind, ChatToolStatus, ChatTruncate, ChatTurnState, ChatUsage,
    chat_event::Change as ChatChange,
};

use super::{TerminalClients, get_client};

/// Sends one `ChatRequest` and returns the host's `ChatResponse`. A refusal is
/// the usual `"<error_code>: <display message>"`.
///
/// Async so a request never blocks the UI thread for the host round trip.
pub async fn chat_request(
    client_id: String,
    request: ChatRequest,
    clients: &TerminalClients,
) -> Result<ChatResponse, String> {
    let client = get_client(clients, &client_id)?;
    let request = envelope_request(request);
    let response = crate::platform::spawn_blocking(move || client.request(request))
        .await
        .map_err(|error| format!("chat request task failed: {error}"))??;
    response
        .chat
        .ok_or_else(|| "host omitted chat response".into())
}

fn envelope_request(chat: ChatRequest) -> v1::Request {
    v1::Request {
        operation: v1::Operation::Chat.into(),
        chat: Some(chat),
        ..Default::default()
    }
}

/// The payload of a chat event frame (kind 20), whose label is the pane id.
pub(super) fn encode_event(event: &ChatEvent) -> Vec<u8> {
    event.encode_to_vec()
}

/// Reads the payload of a chat event frame (kind 20).
pub fn decode_event(payload: &[u8]) -> Result<ChatEvent, String> {
    let event =
        ChatEvent::decode(payload).map_err(|error| format!("chat event is malformed: {error}"))?;
    if event.pane_id.is_empty() || event.change.is_none() {
        return Err("chat event names no pane or carries no change".into());
    }
    Ok(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chat_request_goes_out_as_the_chat_operation() {
        let chat = ChatRequest {
            action: ChatAction::Open.into(),
            pane_id: "%3".into(),
            ..Default::default()
        };
        let request = envelope_request(chat.clone());
        assert_eq!(request.operation, i32::from(v1::Operation::Chat));
        assert_eq!(request.chat, Some(chat));
    }

    #[test]
    fn a_chat_event_survives_its_frame_payload() {
        let event = ChatEvent {
            pane_id: "%3".into(),
            revision: (1 << 53) + 9,
            change: Some(ChatChange::Delta(ChatItemDelta {
                item_id: "a1".into(),
                text_append: "héllo".into(),
            })),
        };
        assert_eq!(decode_event(&encode_event(&event)), Ok(event));
    }

    #[test]
    fn a_payload_that_is_not_a_complete_chat_event_is_refused() {
        assert!(decode_event(&[0xff, 0xff]).is_err());
        // Decodes, but says nothing: an event from a host this build does not
        // understand must not read as a change.
        for incomplete in [
            ChatEvent::default(),
            ChatEvent {
                pane_id: "%3".into(),
                revision: 4,
                change: None,
            },
        ] {
            assert!(decode_event(&encode_event(&incomplete)).is_err());
        }
    }
}
