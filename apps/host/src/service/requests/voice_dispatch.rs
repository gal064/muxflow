//! Voice operations (docs/mobile/voice-mode-plan.md §4.4).
//!
//! Thin: validation and the sidecar live in `service::voice`; this maps a
//! request to a `VoiceService` call and its outcome to a `Response`. Every
//! refusal carries `VoiceResponse { operation_id, retryable }` because
//! `Response` has no retry hint of its own.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tmux_agent_protocol::v1;

use super::super::SequencerControl;
use super::super::voice::{VoiceError, VoiceService};
use super::{response_error, send_response};

pub(super) async fn handle(
    request_id: u64,
    operation: v1::Operation,
    request: v1::Request,
    control_tx: &tokio::sync::mpsc::Sender<SequencerControl>,
    connection_id: u64,
    active_server_identity: &str,
    cancellation: &AtomicBool,
    connection_closed: &AtomicBool,
) {
    let response = match request.voice {
        None => voice_error(
            "",
            "voice_invalid_request",
            "a voice operation needs a voice payload",
            false,
        ),
        Some(voice) => {
            handle_inner(
                operation,
                voice,
                connection_id,
                active_server_identity,
                cancellation,
                connection_closed,
            )
            .await
        }
    };
    send_response(control_tx, request_id, response).await;
}

async fn handle_inner(
    operation: v1::Operation,
    request: v1::VoiceRequest,
    connection_id: u64,
    active_server_identity: &str,
    cancellation: &AtomicBool,
    connection_closed: &AtomicBool,
) -> v1::Response {
    if cancellation.load(Ordering::Acquire) {
        return voice_error(
            &request.operation_id,
            "cancelled",
            "request was cancelled",
            false,
        );
    }
    let operation_id = request.operation_id.clone();
    let outcome = dispatch(
        operation,
        request,
        connection_id,
        active_server_identity,
        cancellation,
        connection_closed,
    )
    .await;
    // The flag is checked again before answering: a Cancel that arrived while
    // the sidecar was working must not be answered with a stale success.
    if cancellation.load(Ordering::Acquire) {
        return voice_error(&operation_id, "cancelled", "request was cancelled", false);
    }
    match outcome {
        Ok(voice) => v1::Response {
            ok: true,
            voice: Some(v1::VoiceResponse {
                operation_id,
                ..voice
            }),
            ..Default::default()
        },
        Err(error) => voice_error(&operation_id, error.code, &error.message, error.retryable),
    }
}

/// Takes the request by value so the utterance (up to 8 MiB) moves into the
/// service instead of being copied.
async fn dispatch(
    operation: v1::Operation,
    request: v1::VoiceRequest,
    connection_id: u64,
    active_server_identity: &str,
    cancellation: &AtomicBool,
    connection_closed: &AtomicBool,
) -> Result<v1::VoiceResponse, VoiceError> {
    let service: Arc<VoiceService> = VoiceService::global();
    match operation {
        v1::Operation::VoiceStatus => {
            let status = service.status();
            if request.warm && status.readiness == v1::VoiceReadiness::Ready as i32 {
                service.warm();
            }
            Ok(v1::VoiceResponse {
                status: Some(status),
                ..Default::default()
            })
        }
        v1::Operation::VoiceSession => {
            if request.pane_id.is_empty() {
                service.clear_sessions(connection_id);
            } else {
                if request.expected_server_identity != active_server_identity {
                    return Err(VoiceError::invalid(
                        "tmux server identity changed; reopen Voice",
                    ));
                }
                service.register_session(
                    connection_id,
                    active_server_identity,
                    &request.pane_id,
                )?;
            }
            Ok(v1::VoiceResponse::default())
        }
        v1::Operation::VoiceProvision => {
            let status = service
                .provision(
                    &request.operation_id,
                    request.confirmed,
                    cancellation,
                    Some(connection_closed),
                    &mut |_| {},
                )
                .await?;
            Ok(v1::VoiceResponse {
                status: Some(status),
                ..Default::default()
            })
        }
        v1::Operation::VoiceTranscribe => {
            let transcript = service
                .transcribe(request.audio, &request.audio_mime, cancellation)
                .await?;
            Ok(v1::VoiceResponse {
                transcript: Some(transcript),
                ..Default::default()
            })
        }
        v1::Operation::VoiceSpeak => {
            let provider = v1::VoiceProvider::try_from(request.provider)
                .map_err(|_| VoiceError::unsupported_provider())?;
            let speech = service
                .speak(&request.text, provider, &request.voice, cancellation)
                .await?;
            Ok(v1::VoiceResponse {
                speech: Some(speech),
                ..Default::default()
            })
        }
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

    #[tokio::test]
    async fn a_request_without_a_voice_payload_is_invalid() {
        let (control_tx, mut control_rx) = tokio::sync::mpsc::channel(1);
        handle(
            9,
            v1::Operation::VoiceStatus,
            v1::Request::default(),
            &control_tx,
            1,
            "server-a",
            &AtomicBool::new(false),
            &AtomicBool::new(false),
        )
        .await;
        let Some(SequencerControl::Response {
            request_id,
            response,
            ..
        }) = control_rx.recv().await
        else {
            panic!("expected a response");
        };
        assert_eq!(request_id, 9);
        assert!(!response.ok);
        assert_eq!(response.error_code, "voice_invalid_request");
        let voice = response.voice.expect("voice payload on every voice answer");
        assert!(!voice.retryable);
    }

    #[tokio::test]
    async fn a_cancelled_voice_request_is_answered_cancelled_with_its_operation_id() {
        let response = handle_inner(
            v1::Operation::VoiceTranscribe,
            v1::VoiceRequest {
                operation_id: "op-7".into(),
                ..Default::default()
            },
            1,
            "server-a",
            &AtomicBool::new(true),
            &AtomicBool::new(false),
        )
        .await;
        assert_eq!(response.error_code, "cancelled");
        let voice = response.voice.unwrap();
        assert_eq!(voice.operation_id, "op-7");
        assert!(!voice.retryable);
    }

    #[tokio::test]
    async fn a_voice_session_cannot_register_across_a_tmux_server_replacement() {
        let response = handle_inner(
            v1::Operation::VoiceSession,
            v1::VoiceRequest {
                pane_id: "%7".into(),
                expected_server_identity: "server-old".into(),
                ..Default::default()
            },
            91,
            "server-new",
            &AtomicBool::new(false),
            &AtomicBool::new(false),
        )
        .await;

        assert!(!response.ok);
        assert_eq!(response.error_code, "voice_invalid_request");
        assert!(
            response
                .display_message
                .contains("tmux server identity changed")
        );
    }

    #[test]
    fn voice_errors_carry_the_operation_id_and_retry_hint() {
        let response = voice_error("op-1", "voice_busy", "busy", true);
        assert!(!response.ok);
        assert_eq!(response.error_code, "voice_busy");
        let voice = response.voice.unwrap();
        assert_eq!(voice.operation_id, "op-1");
        assert!(voice.retryable);
    }
}
