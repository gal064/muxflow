//! Voice operations (docs/mobile/voice-mode-plan.md §4).
//!
//! Wave 1 of the voice feature lands the wire contract only. Every handler here
//! answers `voice_model_missing` so the host compiles against the final enum
//! and the phone gets a truthful, non-retryable answer. Wave 2 replaces the
//! body of [`handle_inner`] with the `VoiceService` calls; the dispatcher arm,
//! the policy rows and this signature stay.

use std::sync::atomic::{AtomicBool, Ordering};

use tmux_agent_protocol::v1;

use super::super::SequencerControl;
use super::{response_error, send_response};

pub(super) async fn handle(
    request_id: u64,
    operation: v1::Operation,
    request: v1::Request,
    control_tx: &tokio::sync::mpsc::Sender<SequencerControl>,
    cancellation: &AtomicBool,
) {
    let response = handle_inner(operation, request.voice.unwrap_or_default(), cancellation);
    send_response(control_tx, request_id, response).await;
}

fn handle_inner(
    operation: v1::Operation,
    request: v1::VoiceRequest,
    cancellation: &AtomicBool,
) -> v1::Response {
    if cancellation.load(Ordering::Acquire) {
        return voice_error(
            &request.operation_id,
            "cancelled",
            "request was cancelled",
            false,
        );
    }
    match operation {
        v1::Operation::VoiceStatus
        | v1::Operation::VoiceProvision
        | v1::Operation::VoiceTranscribe
        | v1::Operation::VoiceSpeak
        | v1::Operation::VoiceSession => voice_error(
            &request.operation_id,
            "voice_model_missing",
            "Voice is not set up on this host yet",
            false,
        ),
        other => unreachable!("{} is not a voice operation", other.as_str_name()),
    }
}

/// An error answer that still carries the caller's correlation id and the
/// retry hint, because `Response` has no `retryable` of its own.
fn voice_error(operation_id: &str, code: &str, message: &str, retryable: bool) -> v1::Response {
    let mut response = response_error(code, message);
    response.voice = Some(v1::VoiceResponse {
        operation_id: operation_id.to_owned(),
        retryable,
        ..Default::default()
    });
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_voice_operation_answers_model_missing_with_the_echoed_operation_id() {
        for operation in [
            v1::Operation::VoiceStatus,
            v1::Operation::VoiceProvision,
            v1::Operation::VoiceTranscribe,
            v1::Operation::VoiceSpeak,
            v1::Operation::VoiceSession,
        ] {
            let response = handle_inner(
                operation,
                v1::VoiceRequest {
                    operation_id: "op-7".into(),
                    ..Default::default()
                },
                &AtomicBool::new(false),
            );
            assert!(!response.ok, "{}", operation.as_str_name());
            assert_eq!(response.error_code, "voice_model_missing");
            let voice = response.voice.expect("voice payload on every voice answer");
            assert_eq!(voice.operation_id, "op-7");
            assert!(!voice.retryable);
        }
    }

    #[test]
    fn a_cancelled_voice_request_is_answered_cancelled() {
        let response = handle_inner(
            v1::Operation::VoiceTranscribe,
            v1::VoiceRequest::default(),
            &AtomicBool::new(true),
        );
        assert_eq!(response.error_code, "cancelled");
        assert!(!response.voice.unwrap().retryable);
    }
}
