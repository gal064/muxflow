//! `OPERATION_SESSION_HISTORY` (docs/design/chat-plan.md §4).
//!
//! The wire only: nothing records sessions yet, so a listing is empty and the
//! actions that need a recorded session are refused. Stream E replaces this
//! with the history store.

use tmux_agent_protocol::v1;

use super::response_error;

pub(super) fn handle(request: Option<v1::HistoryRequest>) -> v1::Response {
    use v1::HistoryAction as Action;
    let action = request.map(|request| Action::try_from(request.action).unwrap_or_default());
    match action {
        None | Some(Action::Unspecified) => response_error(
            "history_invalid_request",
            "A history operation needs a history payload and an action.",
        ),
        Some(Action::List) => v1::Response {
            ok: true,
            history: Some(v1::HistoryResponse::default()),
            ..Default::default()
        },
        Some(Action::Resume | Action::Snooze | Action::Wake) => response_error(
            "history_unavailable",
            "Session history is not recorded on this host yet.",
        ),
    }
}
