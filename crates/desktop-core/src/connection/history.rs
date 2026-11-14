//! Session history and snooze (docs/design/chat-plan.md §4): one request to
//! a host's helper, and the event that says its history changed. Carried as
//! the protocol's own types, like chat.

use prost::Message as _;
use tmux_agent_protocol::v1;

pub use tmux_agent_protocol::v1::{
    HistoryAction, HistoryEntry, HistoryEvent, HistoryRequest, HistoryResponse,
};

use super::{TerminalClients, get_client};

/// Sends one `HistoryRequest` and returns the host's `HistoryResponse`. A
/// refusal is the usual `"<error_code>: <display message>"`.
pub async fn history_request(
    client_id: String,
    request: HistoryRequest,
    clients: &TerminalClients,
) -> Result<HistoryResponse, String> {
    let client = get_client(clients, &client_id)?;
    let request = envelope_request(request);
    let response = crate::platform::spawn_blocking(move || client.request(request))
        .await
        .map_err(|error| format!("history request task failed: {error}"))??;
    response
        .history
        .ok_or_else(|| "host omitted history response".into())
}

fn envelope_request(history: HistoryRequest) -> v1::Request {
    v1::Request {
        operation: v1::Operation::SessionHistory.into(),
        history: Some(history),
        ..Default::default()
    }
}

/// The payload of a history event frame (kind 21).
pub(super) fn encode_event(event: &HistoryEvent) -> Vec<u8> {
    event.encode_to_vec()
}

/// Reads the payload of a history event frame (kind 21).
pub fn decode_event(payload: &[u8]) -> Result<HistoryEvent, String> {
    HistoryEvent::decode(payload).map_err(|error| format!("history event is malformed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_history_request_goes_out_as_the_session_history_operation() {
        let history = HistoryRequest {
            action: HistoryAction::List.into(),
            ..Default::default()
        };
        let request = envelope_request(history.clone());
        assert_eq!(request.operation, i32::from(v1::Operation::SessionHistory));
        assert_eq!(request.history, Some(history));
    }

    #[test]
    fn a_history_event_survives_its_frame_payload() {
        let event = HistoryEvent {
            generation: (1 << 53) + 9,
            woke: vec![HistoryEntry {
                adapter_id: "codex".into(),
                native_session_id: "s1".into(),
                woke: true,
                ..Default::default()
            }],
        };
        assert_eq!(decode_event(&encode_event(&event)), Ok(event));
        assert!(decode_event(&[0xff, 0xff]).is_err());
    }
}
