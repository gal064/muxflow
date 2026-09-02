use std::{fs, path::Path};

use tmux_agent_protocol::v1;
use tokio::sync::mpsc;

use super::sidecar::tests::{ECHO_SIDECAR, fake_sidecar};
use super::*;
use crate::service::{SequencerControl, register_control_event_sink};

fn uv_present() -> Result<PathBuf, tmux_control::ExecutableError> {
    Ok(PathBuf::from("/bin/sh"))
}

fn uv_absent() -> Result<PathBuf, tmux_control::ExecutableError> {
    Err(tmux_control::ExecutableError::NotFound {
        program: "uv",
        override_env: uv::OVERRIDE_ENV,
    })
}

/// A service whose sidecar is the `#!/bin/sh` echo script and whose model is
/// four one-byte files, so every path but the real recognizer runs.
fn service(dir: &Path, idle_after: Duration, script: &str) -> Arc<VoiceService> {
    let script = fake_sidecar(dir, "fake-sidecar.sh", script);
    let service = VoiceService::with_runtime(
        dir.to_path_buf(),
        idle_after,
        uv_present,
        Box::new(move |_, _, _| tokio::process::Command::new(&script)),
    );
    service.model().write_fake_complete();
    Arc::new(service)
}

fn not_cancelled() -> AtomicBool {
    AtomicBool::new(false)
}

const FIXTURE: &[u8] = include_bytes!("fixtures/hello.m4a");

#[tokio::test]
async fn status_reports_uv_model_and_provisioning_without_spawning() {
    let dir = tempfile::tempdir().unwrap();
    let missing_uv = VoiceService::with_runtime(
        dir.path().to_path_buf(),
        DEFAULT_IDLE_AFTER,
        uv_absent,
        Box::new(|_, _, _| tokio::process::Command::new("false")),
    );
    let status = missing_uv.status();
    assert_eq!(status.readiness, v1::VoiceReadiness::UvMissing as i32);
    assert!(status.detail.contains("astral.sh"));

    let no_model = VoiceService::with_runtime(
        dir.path().to_path_buf(),
        DEFAULT_IDLE_AFTER,
        uv_present,
        Box::new(|_, _, _| tokio::process::Command::new("false")),
    );
    let status = no_model.status();
    assert_eq!(status.readiness, v1::VoiceReadiness::ModelMissing as i32);
    assert_eq!(status.model_download_bytes, provision::MODEL_DOWNLOAD_BYTES);
    assert!(!status.sidecar_running);
    assert!(
        !dir.path().join("sidecar.log").exists(),
        "status spawned something"
    );

    let service = service(dir.path(), DEFAULT_IDLE_AFTER, ECHO_SIDECAR);
    assert_eq!(service.status().readiness, v1::VoiceReadiness::Ready as i32);
    service.books.lock().unwrap().provisioning = Some(provision::progress(
        "op",
        provision::PHASE_DOWNLOADING,
        5,
        10,
        "",
    ));
    let status = service.status();
    assert_eq!(status.readiness, v1::VoiceReadiness::Provisioning as i32);
    assert_eq!(status.provision.unwrap().transferred_bytes, 5);
}

#[tokio::test]
async fn transcribe_decodes_the_fixture_loads_once_and_single_lines_the_text() {
    let dir = tempfile::tempdir().unwrap();
    let service = service(dir.path(), DEFAULT_IDLE_AFTER, ECHO_SIDECAR);
    let transcript = service
        .transcribe(FIXTURE.to_vec(), "audio/mp4", "en-US", &not_cancelled())
        .await
        .unwrap();
    assert_eq!(transcript.text, "hello world");
    assert_eq!(transcript.decode_millis, 12);
    assert!((900..=1_150).contains(&transcript.audio_millis));
    assert!(service.status().sidecar_running);
    // The second request reuses the hot child: the echo script numbers replies
    // per process, so the id advancing proves no respawn happened.
    let pid = service.sidecar_pid().await;
    service
        .transcribe(FIXTURE.to_vec(), "audio/mp4", "", &not_cancelled())
        .await
        .unwrap();
    assert_eq!(service.sidecar_pid().await, pid);
    assert_eq!(service.slot.lock().await.child.as_ref().unwrap().next_id, 3);
}

#[tokio::test]
async fn transcribe_validates_before_touching_the_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let service = service(dir.path(), DEFAULT_IDLE_AFTER, ECHO_SIDECAR);
    let cases: Vec<(Vec<u8>, &str, &str, &str)> = vec![
        (Vec::new(), "audio/mp4", "", "voice_invalid_request"),
        (
            vec![0; audio::MAX_AUDIO_BYTES + 1],
            "audio/mp4",
            "",
            "voice_audio_too_large",
        ),
        (FIXTURE.to_vec(), "audio/wav", "", "voice_audio_bad_mime"),
        (
            FIXTURE.to_vec(),
            "audio/mp4",
            "not a tag!",
            "voice_invalid_request",
        ),
        (
            b"garbage".to_vec(),
            "audio/mp4",
            "",
            "voice_audio_undecodable",
        ),
    ];
    for (audio, mime, hint, code) in cases {
        let error = service
            .transcribe(audio, mime, hint, &not_cancelled())
            .await
            .unwrap_err();
        assert_eq!(error.code, code);
        assert!(!error.retryable);
    }
    assert!(
        service.sidecar_pid().await.is_none(),
        "validation spawned the sidecar"
    );
    let cancelled = service
        .transcribe(FIXTURE.to_vec(), "audio/mp4", "", &AtomicBool::new(true))
        .await
        .unwrap_err();
    assert_eq!(cancelled.code, "cancelled");
}

#[tokio::test]
async fn speak_shapes_text_validates_voice_and_returns_the_body() {
    let dir = tempfile::tempdir().unwrap();
    let service = service(dir.path(), DEFAULT_IDLE_AFTER, ECHO_SIDECAR);
    let speech = service
        .speak(
            "# Done\n`ls` works",
            v1::VoiceProvider::Unspecified,
            "",
            &not_cancelled(),
        )
        .await
        .unwrap();
    assert_eq!(speech.audio, b"MP3!!");
    assert_eq!(speech.audio_mime, "audio/mpeg");
    assert_eq!(speech.text, "Done. ls works");
    assert_eq!(speech.voice, DEFAULT_VOICE);
    assert_eq!(speech.provider, v1::VoiceProvider::EdgeTts as i32);
    for (text, voice, code) in [
        ("   ", "", "voice_invalid_request"),
        ("hi", "not a voice", "voice_invalid_request"),
        ("```\ncode\n```", "", "voice_nothing_to_say"),
    ] {
        let error = service
            .speak(text, v1::VoiceProvider::EdgeTts, voice, &not_cancelled())
            .await
            .unwrap_err();
        assert_eq!(error.code, code, "{text:?}");
    }
    // `code omitted.` is speakable, so only a reply that is *nothing* is refused.
    assert!(
        service
            .speak(
                "```\ncode\n```\nsee above",
                v1::VoiceProvider::EdgeTts,
                "",
                &not_cancelled()
            )
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn refusals_are_mapped_by_class_and_leave_the_child_alive() {
    let dir = tempfile::tempdir().unwrap();
    let service = service(dir.path(), DEFAULT_IDLE_AFTER, ECHO_SIDECAR);
    let request = SidecarRequest {
        header: {
            let mut header = serde_json::Map::new();
            header.insert("op".into(), "refuse".into());
            header
        },
        body: Vec::new(),
        bound: Duration::from_secs(5),
        needs_model: false,
    };
    let error = service
        .sidecar_request(request, &not_cancelled(), &mut |_| {})
        .await
        .unwrap_err();
    assert_eq!(error.code, "voice_network_unavailable");
    assert!(error.retryable);
    assert!(service.sidecar_pid().await.is_some());
    assert_eq!(service.status().detail, "offline");
}

#[tokio::test]
async fn a_crash_respawns_and_three_in_a_minute_stop_being_retryable() {
    let dir = tempfile::tempdir().unwrap();
    // A sidecar that dies on its first frame: every request is a crash.
    let service = service(dir.path(), DEFAULT_IDLE_AFTER, "read -r line; exit 3\n");
    let mut codes = Vec::new();
    for _ in 0..3 {
        let error = service
            .speak("hello", v1::VoiceProvider::EdgeTts, "", &not_cancelled())
            .await
            .unwrap_err();
        codes.push((error.code, error.retryable));
        assert!(error.message.contains("sidecar.log"));
    }
    assert_eq!(
        codes,
        [
            ("voice_sidecar_failed", true),
            ("voice_sidecar_failed", true),
            ("voice_sidecar_failed", false),
        ]
    );
    let refused = service
        .speak("hello", v1::VoiceProvider::EdgeTts, "", &not_cancelled())
        .await
        .unwrap_err();
    assert_eq!(refused.code, "voice_sidecar_failed");
    assert!(!refused.retryable);
    assert!(service.sidecar_pid().await.is_none());
}

#[tokio::test]
async fn the_idle_timer_unloads_the_sidecar_and_the_next_request_respawns_it() {
    let dir = tempfile::tempdir().unwrap();
    let service = service(dir.path(), Duration::from_millis(200), ECHO_SIDECAR);
    service
        .speak("hello", v1::VoiceProvider::EdgeTts, "", &not_cancelled())
        .await
        .unwrap();
    let first = service.sidecar_pid().await.expect("hot after a request");
    let gone = async {
        loop {
            if service.sidecar_pid().await.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(3), gone)
        .await
        .expect("the idle timer never unloaded the sidecar");
    assert!(!service.status().sidecar_running);
    service
        .speak("again", v1::VoiceProvider::EdgeTts, "", &not_cancelled())
        .await
        .unwrap();
    let second = service.sidecar_pid().await.expect("respawned lazily");
    assert_ne!(first, second);
    service.shutdown().await;
    assert!(service.sidecar_pid().await.is_none());
}

#[tokio::test]
async fn sessions_are_bounded_cleared_by_connection_and_pruned_when_the_connection_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let service = service(dir.path(), DEFAULT_IDLE_AFTER, ECHO_SIDECAR);
    let (sender, _receiver) = mpsc::channel::<SequencerControl>(4);
    let registration = register_control_event_sink(sender);
    let connection = registration.id;
    for index in 0..MAX_SESSIONS {
        service
            .register_session(connection, &format!("agent-{index}"))
            .unwrap();
    }
    let error = service
        .register_session(connection, "one-too-many")
        .unwrap_err();
    assert_eq!(error.code, "voice_too_many_sessions");
    // Re-registering an existing agent refreshes rather than counts.
    service.register_session(connection, "agent-0").unwrap();
    assert_eq!(
        service.register_session(connection, "").unwrap_err().code,
        "voice_invalid_request"
    );
    assert_eq!(service.session_count(), MAX_SESSIONS);
    service.clear_sessions(connection);
    assert_eq!(service.session_count(), 0);

    service.register_session(connection, "agent-x").unwrap();
    drop(registration);
    assert_eq!(
        service.session_connection("agent-x"),
        None,
        "a closed connection keeps no session"
    );
}

#[tokio::test]
async fn a_pushed_reply_is_synthesized_and_sent_only_to_the_session_connection() {
    let dir = tempfile::tempdir().unwrap();
    let service = service(dir.path(), DEFAULT_IDLE_AFTER, ECHO_SIDECAR);
    let (voice_tx, mut voice_rx) = mpsc::channel::<SequencerControl>(8);
    let (other_tx, mut other_rx) = mpsc::channel::<SequencerControl>(8);
    let voice_registration = register_control_event_sink(voice_tx);
    let _other_registration = register_control_event_sink(other_tx);
    service
        .register_session(voice_registration.id, "agent-7")
        .unwrap();

    // No session for this agent: nothing is spoken, nothing is sent.
    service.push_reply(AgentReply {
        agent_id: "agent-other".into(),
        text: "ignored".into(),
        truncated: false,
        state_generation: 1,
        occurred_at_unix_millis: 1,
    });
    service.push_reply(AgentReply {
        agent_id: "agent-7".into(),
        text: "## Done\n\nAll **tests** pass.".into(),
        truncated: true,
        state_generation: 42,
        occurred_at_unix_millis: 1_700_000_000_000,
    });
    let message = tokio::time::timeout(Duration::from_secs(5), voice_rx.recv())
        .await
        .expect("the reply never reached the session connection")
        .unwrap();
    let SequencerControl::OrderedEvent(event) = message else {
        panic!("expected an ordered event");
    };
    assert_eq!(event.kind, v1::EventKind::VoiceReply as i32);
    assert_eq!(event.scope, "agent-7");
    let reply = event.voice.unwrap().reply.unwrap();
    assert_eq!(reply.audio, b"MP3!!");
    assert_eq!(reply.text, "Done. All tests pass.");
    assert!(reply.truncated);
    assert_eq!(reply.agent_id, "agent-7");
    assert_eq!(reply.state_generation, 42);
    assert_eq!(reply.reply_at_unix_millis, 1_700_000_000_000);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        other_rx.try_recv().is_err(),
        "the reply leaked to another connection"
    );
    assert!(
        voice_rx.try_recv().is_err(),
        "an agent without a session produced a push"
    );
}

#[tokio::test]
async fn a_failed_synthesis_still_pushes_the_text_with_the_error_in_status() {
    let dir = tempfile::tempdir().unwrap();
    // Every op is refused as a network failure.
    let service = service(
        dir.path(),
        DEFAULT_IDLE_AFTER,
        r#"while IFS= read -r line; do id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p'); printf '{"id":%s,"ok":false,"class":"network","error":"offline"}\n' "$id"; done
"#,
    );
    let (voice_tx, mut voice_rx) = mpsc::channel::<SequencerControl>(8);
    let registration = register_control_event_sink(voice_tx);
    service
        .register_session(registration.id, "agent-1")
        .unwrap();
    service.push_reply(AgentReply {
        agent_id: "agent-1".into(),
        text: "Finished.".into(),
        truncated: false,
        state_generation: 3,
        occurred_at_unix_millis: 9,
    });
    let SequencerControl::OrderedEvent(event) =
        tokio::time::timeout(Duration::from_secs(5), voice_rx.recv())
            .await
            .unwrap()
            .unwrap()
    else {
        panic!("expected an ordered event");
    };
    let voice = event.voice.unwrap();
    let reply = voice.reply.unwrap();
    assert!(reply.audio.is_empty());
    assert!(reply.audio_mime.is_empty());
    assert_eq!(reply.text, "Finished.");
    assert!(
        voice
            .status
            .unwrap()
            .detail
            .contains("voice_network_unavailable")
    );
}

#[tokio::test]
async fn provision_needs_consent_runs_the_sidecar_and_verifies_by_loading() {
    let dir = tempfile::tempdir().unwrap();
    // The echo script answers `provision` with two progress events then ok
    // (see `progress`), and `load` with ok; the fake model dir is written by
    // hand since no download happens.
    let script = ECHO_SIDECAR.replace("progress) printf", "provision) printf");
    let service = service(dir.path(), DEFAULT_IDLE_AFTER, &script);
    let mut seen = Vec::new();
    let mut record = |progress: &v1::VoiceProvisionProgress| {
        seen.push((progress.phase.clone(), progress.transferred_bytes));
    };
    let refused = service
        .provision("op-1", false, &not_cancelled(), &mut record)
        .await
        .unwrap_err();
    assert_eq!(refused.code, "voice_consent_required");

    // Already complete: idempotent READY, no sidecar.
    let status = service
        .provision("op-1", true, &not_cancelled(), &mut record)
        .await
        .unwrap();
    assert_eq!(status.readiness, v1::VoiceReadiness::Ready as i32);
    assert!(service.sidecar_pid().await.is_none());

    service.model().remove();
    let outcome = service
        .provision("op-2", true, &not_cancelled(), &mut record)
        .await;
    // The fake never wrote the model, so verification fails and cleans up.
    let error = outcome.unwrap_err();
    assert_eq!(error.code, "voice_provision_failed");
    assert!(error.retryable);
    assert_eq!(
        seen,
        [
            ("installing_runtime".to_owned(), 0),
            ("downloading".to_owned(), 1),
            ("downloading".to_owned(), 2),
            ("verifying".to_owned(), 0),
            ("failed".to_owned(), 0),
        ]
    );
    assert!(service.books.lock().unwrap().provisioning.is_none());
    assert_eq!(
        service.status().readiness,
        v1::VoiceReadiness::ModelMissing as i32
    );

    // With the model in place after the "download", verification passes.
    seen.clear();
    let mut record = |progress: &v1::VoiceProvisionProgress| {
        if progress.phase == provision::PHASE_DOWNLOADING {
            service.model().write_fake_complete();
        }
        seen.push((progress.phase.clone(), progress.transferred_bytes));
    };
    let status = service
        .provision("op-3", true, &not_cancelled(), &mut record)
        .await
        .unwrap();
    assert_eq!(status.readiness, v1::VoiceReadiness::Ready as i32);
    assert_eq!(seen.last().map(|(phase, _)| phase.as_str()), Some("ready"));
    assert!(status.sidecar_running);
    assert!(fs::read_dir(dir.path()).unwrap().count() > 0);
}

#[test]
fn language_tags_and_voice_ids_are_validated_by_shape() {
    for tag in ["en", "en-US", "pt-BR", "zh-Hans-CN", "x-en"] {
        assert!(valid_language_tag(tag), "{tag}");
    }
    for tag in ["", "e", "en_US", "en-", "english-language", "en US"] {
        assert!(!valid_language_tag(tag), "{tag}");
    }
    for voice in [
        "en-US-AvaNeural",
        "en-US-AvaMultilingualNeural",
        "zh-CN-XiaoxiaoNeural",
    ] {
        assert!(valid_voice_id(voice), "{voice}");
    }
    for voice in [
        "",
        "AvaNeural",
        "en-US-Ava",
        "en-US-Ava Neural",
        "en-US-<Neural",
    ] {
        assert!(!valid_voice_id(voice), "{voice}");
    }
}
