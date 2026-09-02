//! Voice mode on the host (docs/mobile/voice-mode-plan.md §4).
//!
//! [`VoiceService`] is process-global like `AgentRuntime`: the phone reconnects
//! constantly and the hot sidecar must survive that. Nothing here exists until
//! the first voice request, so a host that never opens the Voice screen pays
//! nothing — no sidecar, no timer, no cache directory.

use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use tmux_agent_protocol::v1;

use super::{broadcast_control_event, control_sink_alive, send_control_event_to};

pub(crate) mod audio;
pub(crate) mod provision;
pub(crate) mod sidecar;
pub(crate) mod text;
pub(crate) mod uv;

use provision::ModelLayout;
use sidecar::{SidecarChild, SidecarFailure};

pub(crate) const DEFAULT_VOICE: &str = "en-US-AvaNeural";
const DEFAULT_IDLE_AFTER: Duration = Duration::from_secs(300);
const IDLE_ENV: &str = "MUXFLOW_VOICE_IDLE_SECS";
/// Requests allowed to wait for the one in-flight sidecar request.
const MAX_WAITERS: usize = 4;
/// Crashes inside the window past which a failure is no longer retryable.
const CRASH_WINDOW: Duration = Duration::from_secs(60);
const CRASH_LIMIT: usize = 3;
const LOAD_TIMEOUT: Duration = Duration::from_secs(120);
const PROVISION_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
pub(crate) const MAX_SESSIONS: usize = 8;
const SESSION_TTL: Duration = Duration::from_secs(10 * 60);
/// SPEAK input bound: a raw reply the hook forwarded (32 KiB) must replay.
const MAX_SPEAK_CHARS: usize = 32 * 1024;

/// One refusal, as the wire wants it: a code from §4.6, a message, and whether
/// the same request could succeed if repeated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VoiceError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
    pub(crate) retryable: bool,
    /// The sidecar itself judged the model files bad (`class: "model"`), as
    /// opposed to crashing, timing out or being cancelled while loading them.
    /// Not on the wire; provisioning uses it to decide whether a download is
    /// worth keeping.
    model_fault: bool,
}

impl VoiceError {
    fn new(code: &'static str, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
            model_fault: false,
        }
    }

    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::new("voice_invalid_request", message, false)
    }

    pub(crate) fn unsupported_provider() -> Self {
        Self::new(
            "voice_provider_unsupported",
            "only Edge TTS is available on this host",
            false,
        )
    }

    fn cancelled() -> Self {
        Self::new("cancelled", "request was cancelled", false)
    }
}

impl std::fmt::Display for VoiceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

/// What ingest hands over when an agent's turn ends with a message
/// (docs/mobile/voice-mode-plan.md §4.5). Consumed, never stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentReply {
    pub(crate) agent_id: String,
    pub(crate) text: String,
    pub(crate) truncated: bool,
    pub(crate) state_generation: u64,
    pub(crate) occurred_at_unix_millis: i64,
}

struct Slot {
    child: Option<SidecarChild>,
    loaded: bool,
}

struct Session {
    connection_id: u64,
    since: Instant,
}

#[derive(Default)]
struct Bookkeeping {
    last_used: Option<Instant>,
    crashes: VecDeque<Instant>,
    last_error: String,
    provisioning: Option<v1::VoiceProvisionProgress>,
    idle_task_started: bool,
    /// The sidecar refused to load the model on disk (`class: "model"`).
    /// Cleared by a load that succeeds; until then STATUS says MODEL_MISSING.
    model_rejected: bool,
}

type UvLookup = fn() -> Result<PathBuf, tmux_control::ExecutableError>;
type CommandFactory = dyn Fn(&Path, &Path, &Path) -> tokio::process::Command + Send + Sync;

pub(crate) struct VoiceService {
    cache_dir: PathBuf,
    idle_after: Duration,
    /// How uv is found; tests substitute a constant so no PATH is consulted.
    uv_lookup: UvLookup,
    /// Builds the sidecar command from (uv, script, cache dir); tests hand
    /// back a `#!/bin/sh` stand-in instead of `uv run`.
    command_factory: Box<CommandFactory>,
    slot: tokio::sync::Mutex<Slot>,
    /// The model is loaded in a live sidecar: what `VoiceStatus.sidecar_running`
    /// reports. Kept beside the slot because the slot is locked for the whole
    /// of a request, including a cold `uv run`, and "busy" is not "hot".
    hot: AtomicBool,
    /// A warm-up task is in flight; a second STATUS `warm` before it finishes
    /// must not queue another waiter behind the same cold start.
    warming: AtomicBool,
    /// The live child's process group, so shutdown can reach it while a
    /// request holds the slot. Zero when there is no child.
    child_pid: std::sync::atomic::AtomicI32,
    waiters: AtomicUsize,
    books: Mutex<Bookkeeping>,
    sessions: Mutex<HashMap<String, Session>>,
}

static GLOBAL: OnceLock<Arc<VoiceService>> = OnceLock::new();

impl VoiceService {
    pub(crate) fn global() -> Arc<Self> {
        Arc::clone(GLOBAL.get_or_init(|| {
            let idle_after = std::env::var(IDLE_ENV)
                .ok()
                .and_then(|value| value.trim().parse::<u64>().ok())
                .map_or(DEFAULT_IDLE_AFTER, Duration::from_secs);
            Arc::new(Self::new(crate::paths::voice_cache_dir(), idle_after))
        }))
    }

    /// The service if a voice request ever created it; a `Stop` for a host
    /// that never opened the Voice screen must not create one.
    fn existing() -> Option<Arc<Self>> {
        GLOBAL.get().cloned()
    }

    pub(crate) fn new(cache_dir: PathBuf, idle_after: Duration) -> Self {
        Self::with_runtime(
            cache_dir,
            idle_after,
            uv::uv_executable,
            Box::new(SidecarChild::uv_command),
        )
    }

    fn with_runtime(
        cache_dir: PathBuf,
        idle_after: Duration,
        uv_lookup: UvLookup,
        command_factory: Box<CommandFactory>,
    ) -> Self {
        Self {
            cache_dir,
            idle_after,
            uv_lookup,
            command_factory,
            slot: tokio::sync::Mutex::new(Slot {
                child: None,
                loaded: false,
            }),
            hot: AtomicBool::new(false),
            warming: AtomicBool::new(false),
            child_pid: std::sync::atomic::AtomicI32::new(0),
            waiters: AtomicUsize::new(0),
            books: Mutex::new(Bookkeeping::default()),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    fn model(&self) -> ModelLayout {
        ModelLayout::in_cache(&self.cache_dir)
    }

    // --- STATUS ---------------------------------------------------------

    /// Readiness right now. Answers at once and spawns nothing.
    pub(crate) fn status(&self) -> v1::VoiceStatus {
        let model = self.model();
        // The filesystem and PATH are consulted before the books are locked:
        // nothing blocking happens under that mutex.
        let uv = (self.uv_lookup)();
        let complete = model.complete();
        let (last_error, provisioning, model_rejected) = {
            let books = self.books.lock().unwrap();
            (
                books.last_error.clone(),
                books.provisioning.clone(),
                books.model_rejected,
            )
        };
        let mut status = v1::VoiceStatus {
            model_dir: model.dir().to_string_lossy().into_owned(),
            model_download_bytes: provision::MODEL_DOWNLOAD_BYTES,
            detail: bounded_detail(&last_error),
            sidecar_running: self.hot.load(Ordering::Acquire),
            ..Default::default()
        };
        match uv {
            Err(error) => {
                status.readiness = v1::VoiceReadiness::UvMissing.into();
                status.detail = uv::install_hint(&error);
                return status;
            }
            Ok(path) => status.uv_path = path.to_string_lossy().into_owned(),
        }
        if let Some(progress) = provisioning {
            status.readiness = v1::VoiceReadiness::Provisioning.into();
            status.provision = Some(progress);
        } else if complete && !model_rejected {
            status.readiness = v1::VoiceReadiness::Ready.into();
        } else {
            status.readiness = v1::VoiceReadiness::ModelMissing.into();
        }
        status
    }

    /// Spawns the sidecar and loads the model on a detached task; a no-op
    /// when it is already hot.
    pub(crate) fn warm(self: &Arc<Self>) {
        if self.hot.load(Ordering::Acquire)
            || self
                .warming
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return;
        }
        let service = Arc::clone(self);
        tokio::spawn(async move {
            let _ = service.ensure_loaded(&AtomicBool::new(false), None).await;
            service.warming.store(false, Ordering::Release);
        });
    }

    // --- SESSION --------------------------------------------------------

    pub(crate) fn register_session(
        &self,
        connection_id: u64,
        agent_id: &str,
    ) -> Result<(), VoiceError> {
        if agent_id.is_empty()
            || agent_id.chars().count() > 256
            || agent_id.chars().any(char::is_control)
        {
            return Err(VoiceError::invalid(
                "agent_id must be 1-256 printable characters",
            ));
        }
        let mut sessions = self.sessions.lock().unwrap();
        prune_sessions(&mut sessions);
        if !sessions.contains_key(agent_id) && sessions.len() >= MAX_SESSIONS {
            return Err(VoiceError::new(
                "voice_too_many_sessions",
                format!("at most {MAX_SESSIONS} voice sessions can be registered at once"),
                false,
            ));
        }
        sessions.insert(
            agent_id.to_owned(),
            Session {
                connection_id,
                since: Instant::now(),
            },
        );
        Ok(())
    }

    /// `agent_id = ""`: every session this connection registered ends.
    pub(crate) fn clear_sessions(&self, connection_id: u64) {
        self.sessions
            .lock()
            .unwrap()
            .retain(|_, session| session.connection_id != connection_id);
    }

    #[cfg(test)]
    fn session_count(&self) -> usize {
        self.sessions.lock().unwrap().len()
    }

    fn session_connection(&self, agent_id: &str) -> Option<u64> {
        let mut sessions = self.sessions.lock().unwrap();
        prune_sessions(&mut sessions);
        sessions.get(agent_id).map(|session| session.connection_id)
    }

    // --- pushed replies -------------------------------------------------

    /// Synthesizes the reply on a detached task and pushes it to the one
    /// connection holding a session for the agent. Nothing is retained: an
    /// agent without a session costs a map lookup.
    pub(crate) fn push_reply(self: &Arc<Self>, reply: AgentReply) {
        if self.session_connection(&reply.agent_id).is_none() {
            return;
        }
        let service = Arc::clone(self);
        tokio::spawn(async move {
            let spoken = text::speech_text(&reply.text, text::SPEECH_CAP_CHARS);
            if spoken.text.is_empty() {
                return;
            }
            let synthesized = service
                .synthesize(&spoken.text, DEFAULT_VOICE, &AtomicBool::new(false))
                .await;
            let (audio, status) = match synthesized {
                Ok(audio) => (audio, None),
                Err(error) => (
                    Vec::new(),
                    Some(v1::VoiceStatus {
                        detail: bounded_detail(&error.to_string()),
                        ..service.status()
                    }),
                ),
            };
            let Some(connection_id) = service.session_connection(&reply.agent_id) else {
                return;
            };
            let event = v1::HostEvent {
                kind: v1::EventKind::VoiceReply.into(),
                scope: reply.agent_id.clone(),
                voice: Some(v1::VoiceEvent {
                    status,
                    reply: Some(v1::VoiceSpeech {
                        audio_mime: if audio.is_empty() {
                            String::new()
                        } else {
                            "audio/mpeg".into()
                        },
                        audio,
                        text: spoken.text,
                        truncated: spoken.truncated || reply.truncated,
                        voice: DEFAULT_VOICE.into(),
                        provider: v1::VoiceProvider::EdgeTts.into(),
                        agent_id: reply.agent_id.clone(),
                        state_generation: reply.state_generation,
                        reply_at_unix_millis: reply.occurred_at_unix_millis,
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            };
            // A full queue is a burst of terminal output, not a departed
            // phone: only a connection that is gone loses its sessions.
            if send_control_event_to(connection_id, event) == super::events::Delivery::Gone {
                service
                    .sessions
                    .lock()
                    .unwrap()
                    .retain(|_, session| session.connection_id != connection_id);
            }
        });
    }

    // --- PROVISION ------------------------------------------------------

    /// Downloads and verifies the model. Progress is broadcast as
    /// `EVENT_KIND_VOICE_PROVISION` and handed to `on_progress` (for the CLI).
    /// `connection_closed` tells a Cancel raised by the connection's own
    /// teardown apart from one the phone sent: the phone reconnects
    /// constantly, and a dropped link must not throw away a half-fetched
    /// model. Only an explicit Cancel reaches the sidecar; a download whose
    /// connection went away runs on, and the reconnecting phone's STATUS
    /// resumes the progress bar (§4.4).
    pub(crate) async fn provision(
        self: &Arc<Self>,
        operation_id: &str,
        confirmed: bool,
        cancel: &AtomicBool,
        connection_closed: Option<&AtomicBool>,
        on_progress: &mut (dyn FnMut(&v1::VoiceProvisionProgress) + Send),
    ) -> Result<v1::VoiceStatus, VoiceError> {
        if !confirmed {
            return Err(VoiceError::new(
                "voice_consent_required",
                "the speech model download needs explicit consent",
                false,
            ));
        }
        if let Err(error) = (self.uv_lookup)() {
            return Err(VoiceError::new(
                "voice_uv_missing",
                uv::install_hint(&error),
                false,
            ));
        }
        let model = self.model();
        // Filesystem first, lock second (see `status`).
        let complete = model.complete();
        let already_complete = {
            let mut books = self.books.lock().unwrap();
            if books.provisioning.is_some() {
                return Err(VoiceError::new(
                    "voice_provisioning",
                    "a provision is already running",
                    true,
                ));
            }
            if complete {
                true
            } else {
                books.provisioning = Some(provision::progress(
                    operation_id,
                    provision::PHASE_INSTALLING_RUNTIME,
                    0,
                    0,
                    "",
                ));
                false
            }
        };
        if already_complete {
            // Not a plain no-op: verifying is what turns a model the sidecar
            // has rejected — corrupted on disk, or outgrown by a pin bump —
            // back into a fresh download on the next attempt.
            return match self.ensure_loaded(cancel, connection_closed).await {
                Ok(()) => Ok(self.status()),
                Err(error) if error.model_fault => {
                    model.remove();
                    Err(VoiceError::new(
                        "voice_provision_failed",
                        error.message,
                        true,
                    ))
                }
                Err(error) => Err(error),
            };
        }
        let outcome = self
            .provision_inner(operation_id, &model, cancel, connection_closed, on_progress)
            .await;
        let final_progress = match &outcome {
            Ok(()) => provision::progress(operation_id, provision::PHASE_READY, 0, 0, ""),
            Err(error) => {
                // The sidecar removes its own partials when it gets to; a
                // sidecar the host killed did not, so sweep here as well. Not
                // for the refusals answered before anything was attempted.
                if !matches!(
                    error.code,
                    "voice_busy" | "voice_uv_missing" | "voice_provisioning"
                ) {
                    model.sweep_partials();
                }
                provision::progress(operation_id, provision::PHASE_FAILED, 0, 0, &error.message)
            }
        };
        self.books.lock().unwrap().provisioning = None;
        self.emit_progress(&final_progress, on_progress);
        outcome.map(|()| self.status())
    }

    async fn provision_inner(
        self: &Arc<Self>,
        operation_id: &str,
        model: &ModelLayout,
        cancel: &AtomicBool,
        connection_closed: Option<&AtomicBool>,
        on_progress: &mut (dyn FnMut(&v1::VoiceProvisionProgress) + Send),
    ) -> Result<(), VoiceError> {
        let installing =
            provision::progress(operation_id, provision::PHASE_INSTALLING_RUNTIME, 0, 0, "");
        self.emit_progress(&installing, on_progress);
        let mut forward = |event: &serde_json::Value| {
            let phase = match event.get("phase").and_then(serde_json::Value::as_str) {
                Some("extracting") => provision::PHASE_EXTRACTING,
                _ => provision::PHASE_DOWNLOADING,
            };
            let progress = provision::progress(
                operation_id,
                phase,
                event
                    .get("transferred")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                event
                    .get("total")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                "",
            );
            self.emit_progress(&progress, on_progress);
        };
        let request = SidecarRequest {
            header: model.provision_header(),
            body: Vec::new(),
            bound: PROVISION_TIMEOUT,
            needs_model: false,
            cancellable: true,
            invalidates_model: true,
        };
        self.sidecar_request(request, cancel, connection_closed, &mut forward)
            .await
            .map_err(|error| match error.code {
                // Not about the download: the phone should show these as they are.
                "cancelled" | "voice_busy" | "voice_uv_missing" | "voice_sidecar_timeout" => error,
                // A refused, crashed or otherwise failed download or extraction
                // (§4.6): retryable — a bad archive is fetched again — unless
                // the sidecar itself has crashed out of its retry window.
                _ => VoiceError::new(
                    "voice_provision_failed",
                    error.message,
                    error.retryable || error.model_fault,
                ),
            })?;
        if !model.complete() {
            // The sidecar said it finished, yet the layout is not there: the
            // archive did not contain what it should have.
            model.remove();
            return Err(VoiceError::new(
                "voice_provision_failed",
                "the download did not produce a complete model",
                true,
            ));
        }
        let verifying = provision::progress(operation_id, provision::PHASE_VERIFYING, 0, 0, "");
        self.emit_progress(&verifying, on_progress);
        if let Err(error) = self.ensure_loaded(cancel, connection_closed).await {
            if error.model_fault {
                // The sidecar read the files and rejected them: a corrupt
                // download, and the next provision must start from nothing
                // rather than verify the same bytes.
                model.remove();
                return Err(VoiceError::new(
                    "voice_provision_failed",
                    error.message,
                    true,
                ));
            }
            // Cancelled, crashed or timed out while loading: the bytes are fine
            // as far as anyone knows. The next STATUS finds a complete model
            // and the next request verifies it, without another 487 MB.
            return Err(error);
        }
        Ok(())
    }

    fn emit_progress(
        &self,
        progress: &v1::VoiceProvisionProgress,
        on_progress: &mut (dyn FnMut(&v1::VoiceProvisionProgress) + Send),
    ) {
        if let Some(cached) = self.books.lock().unwrap().provisioning.as_mut() {
            *cached = progress.clone();
        }
        on_progress(progress);
        broadcast_control_event(v1::HostEvent {
            kind: v1::EventKind::VoiceProvision.into(),
            scope: "voice".into(),
            voice: Some(v1::VoiceEvent {
                provision: Some(progress.clone()),
                ..Default::default()
            }),
            ..Default::default()
        });
    }

    // --- TRANSCRIBE -----------------------------------------------------

    pub(crate) async fn transcribe(
        self: &Arc<Self>,
        audio: Vec<u8>,
        mime: &str,
        language_hint: &str,
        cancel: &AtomicBool,
    ) -> Result<v1::VoiceTranscript, VoiceError> {
        if audio.is_empty() {
            return Err(VoiceError::invalid("audio is required"));
        }
        if audio.len() > audio::MAX_AUDIO_BYTES {
            return Err(VoiceError::new(
                "voice_audio_too_large",
                format!("audio exceeds the {}-byte limit", audio::MAX_AUDIO_BYTES),
                false,
            ));
        }
        if !audio::accepted_mime(mime) {
            return Err(VoiceError::new(
                "voice_audio_bad_mime",
                "audio_mime must be audio/mp4, audio/x-m4a or audio/aac",
                false,
            ));
        }
        if !language_hint.is_empty() && !valid_language_tag(language_hint) {
            return Err(VoiceError::invalid("language_hint must be a BCP-47 tag"));
        }
        self.readiness_gate()?;
        let decode_started = Instant::now();
        let mime_owned = mime.to_owned();
        let pcm =
            tokio::task::spawn_blocking(move || audio::decode_to_mono_f32(audio, &mime_owned))
                .await
                .map_err(|error| {
                    VoiceError::new("voice_audio_undecodable", error.to_string(), false)
                })?
                .map_err(|error| match error {
                    audio::AudioError::TooLong => VoiceError::new(
                        "voice_audio_too_long",
                        format!("audio is longer than {} ms", audio::MAX_AUDIO_MILLIS),
                        false,
                    ),
                    audio::AudioError::Undecodable(detail) => {
                        VoiceError::new("voice_audio_undecodable", detail, false)
                    }
                })?;
        let decode_elapsed = decode_started.elapsed();
        let audio_millis = pcm.duration_millis();
        if audio_millis < audio::MIN_AUDIO_MILLIS {
            return Err(VoiceError::new(
                "voice_audio_too_short",
                format!("audio is shorter than {} ms", audio::MIN_AUDIO_MILLIS),
                false,
            ));
        }
        if audio_millis > audio::MAX_AUDIO_MILLIS {
            return Err(VoiceError::new(
                "voice_audio_too_long",
                format!("audio is longer than {} ms", audio::MAX_AUDIO_MILLIS),
                false,
            ));
        }
        let mut header = serde_json::Map::new();
        header.insert("op".into(), "transcribe".into());
        header.insert("sample_rate".into(), pcm.sample_rate.into());
        if !language_hint.is_empty() {
            header.insert("language".into(), language_hint.into());
        }
        let body: Vec<u8> = pcm.samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let request = SidecarRequest {
            header,
            body,
            bound: Duration::from_secs(30) + Duration::from_millis(u64::from(audio_millis) * 2),
            needs_model: true,
            cancellable: false,
            invalidates_model: false,
        };
        let sidecar_started = Instant::now();
        let reply = self
            .sidecar_request(request, cancel, None, &mut |_| {})
            .await?;
        let text = text::transcript_line(
            reply
                .header
                .get("text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default(),
        );
        let decode_millis = reply
            .header
            .get("decode_millis")
            .and_then(serde_json::Value::as_u64)
            .and_then(|millis| u32::try_from(millis).ok())
            .unwrap_or(0);
        crate::diagnostics::write_voice_timing_log(
            "transcribe",
            audio_millis,
            Some(decode_elapsed),
            sidecar_started.elapsed(),
            decode_millis,
        );
        Ok(v1::VoiceTranscript {
            text,
            audio_millis,
            decode_millis,
        })
    }

    // --- SPEAK ----------------------------------------------------------

    pub(crate) async fn speak(
        self: &Arc<Self>,
        text: &str,
        provider: v1::VoiceProvider,
        voice: &str,
        cancel: &AtomicBool,
    ) -> Result<v1::VoiceSpeech, VoiceError> {
        if !matches!(
            provider,
            v1::VoiceProvider::Unspecified | v1::VoiceProvider::EdgeTts
        ) {
            return Err(VoiceError::unsupported_provider());
        }
        if text.trim().is_empty() {
            return Err(VoiceError::invalid("text is required"));
        }
        if text.chars().count() > MAX_SPEAK_CHARS {
            return Err(VoiceError::invalid(format!(
                "text exceeds {MAX_SPEAK_CHARS} characters"
            )));
        }
        let voice = if voice.is_empty() {
            DEFAULT_VOICE
        } else {
            voice
        };
        if !valid_voice_id(voice) {
            return Err(VoiceError::invalid(
                "voice must be an Edge TTS voice identifier",
            ));
        }
        let spoken = text::speech_text(text, text::SPEECH_CAP_CHARS);
        if spoken.text.is_empty() {
            return Err(VoiceError::new(
                "voice_nothing_to_say",
                "the text has nothing speakable in it",
                false,
            ));
        }
        let audio = self.synthesize(&spoken.text, voice, cancel).await?;
        Ok(v1::VoiceSpeech {
            audio,
            audio_mime: "audio/mpeg".into(),
            text: spoken.text,
            truncated: spoken.truncated,
            voice: voice.to_owned(),
            provider: v1::VoiceProvider::EdgeTts.into(),
            ..Default::default()
        })
    }

    /// Already-shaped speech text to MP3 bytes.
    async fn synthesize(
        self: &Arc<Self>,
        spoken: &str,
        voice: &str,
        cancel: &AtomicBool,
    ) -> Result<Vec<u8>, VoiceError> {
        self.readiness_gate()?;
        let mut header = serde_json::Map::new();
        header.insert("op".into(), "speak".into());
        header.insert("text".into(), spoken.into());
        header.insert("voice".into(), voice.into());
        let request = SidecarRequest {
            header,
            body: Vec::new(),
            bound: Duration::from_secs(20)
                + Duration::from_millis(10 * spoken.chars().count() as u64),
            // Speech does not need the recognizer, but a host without the
            // model is not "set up" for voice; §4.6 answers model_missing.
            needs_model: false,
            cancellable: false,
            invalidates_model: false,
        };
        let started = Instant::now();
        let reply = self
            .sidecar_request(request, cancel, None, &mut |_| {})
            .await?;
        crate::diagnostics::write_voice_timing_log("speak", 0, None, started.elapsed(), 0);
        if reply.body.is_empty() {
            return Err(VoiceError::new(
                "voice_network_unavailable",
                "edge-tts returned no audio",
                true,
            ));
        }
        Ok(reply.body)
    }

    /// uv and the model must both be there before a sidecar is worth
    /// spawning; each absence has its own non-retryable code.
    fn readiness_gate(&self) -> Result<(), VoiceError> {
        if let Err(error) = (self.uv_lookup)() {
            return Err(VoiceError::new(
                "voice_uv_missing",
                uv::install_hint(&error),
                false,
            ));
        }
        if self.books.lock().unwrap().provisioning.is_some() {
            return Err(VoiceError::new(
                "voice_provisioning",
                "the speech model is still being provisioned",
                true,
            ));
        }
        if !self.model().complete() {
            return Err(VoiceError::new(
                "voice_model_missing",
                "the speech model is not provisioned on this host",
                false,
            ));
        }
        Ok(())
    }

    // --- sidecar --------------------------------------------------------

    async fn ensure_loaded(
        self: &Arc<Self>,
        cancel: &AtomicBool,
        connection_closed: Option<&AtomicBool>,
    ) -> Result<(), VoiceError> {
        let request = SidecarRequest {
            header: serde_json::Map::new(),
            body: Vec::new(),
            bound: Duration::ZERO,
            needs_model: true,
            cancellable: false,
            invalidates_model: false,
        };
        self.sidecar_request(request, cancel, connection_closed, &mut |_| {})
            .await
            .map(|_| ())
    }

    /// One in-flight sidecar request at a time, at most [`MAX_WAITERS`]
    /// behind it. Spawns the child and loads the model when needed; a crash
    /// drops the child so the next request respawns it.
    async fn sidecar_request(
        self: &Arc<Self>,
        request: SidecarRequest,
        cancel: &AtomicBool,
        connection_closed: Option<&AtomicBool>,
        on_progress: &mut (dyn FnMut(&serde_json::Value) + Send),
    ) -> Result<sidecar::Reply, VoiceError> {
        // The count includes the request in flight, so "more than
        // MAX_WAITERS waiting" is a previous value above the bound.
        if self.waiters.fetch_add(1, Ordering::AcqRel) > MAX_WAITERS {
            self.waiters.fetch_sub(1, Ordering::AcqRel);
            return Err(VoiceError::new(
                "voice_busy",
                "too many voice requests are waiting; try again",
                true,
            ));
        }
        let _waiting = WaiterGuard(&self.waiters);
        // One notion of "cancelled" for every check below: a Cancel the phone
        // sent, not the token the connection's own teardown raises. A request
        // whose connection went away runs on (see `provision`); its answer has
        // nowhere to go, and a provision must not be lost to a flapping link.
        let cancelled = || {
            cancel.load(Ordering::Acquire)
                && !connection_closed.is_some_and(|closed| closed.load(Ordering::Acquire))
        };
        let queued = Instant::now();
        let mut slot = self.slot.lock().await;
        crate::diagnostics::write_voice_timing_log("queue_wait", 0, None, queued.elapsed(), 0);
        if cancelled() {
            return Err(VoiceError::cancelled());
        }
        if slot.child.is_none() {
            self.spawn_into(&mut slot)?;
            // The first frame after a spawn waits on `uv run` resolving the
            // interpreter and wheels: give that the cold-start allowance
            // (§4.3, 120 s) rather than charging it to the request's own bound
            // — or the request's bound when that is longer, so a provision's
            // `installing_runtime` phase has the same two hours as its download.
            // An explicit Cancel during that wait is honoured for a
            // cancellable op: uv never acknowledges it, so the grace expires
            // and the half-started child is killed as the cancel's outcome.
            let child = slot.child.as_mut().expect("spawned above");
            if let Err(failure) = child
                .request(
                    ping_header(),
                    &[],
                    LOAD_TIMEOUT.max(request.bound),
                    request
                        .cancellable
                        .then_some(&cancelled as &(dyn Fn() -> bool + Sync)),
                    &mut |_| {},
                )
                .await
            {
                return Err(self.handle_failure(&mut slot, failure).await);
            }
        }
        if request.needs_model && !slot.loaded {
            let model = self.model();
            if !model.complete() {
                return Err(VoiceError::new(
                    "voice_model_missing",
                    "the speech model is not provisioned on this host",
                    false,
                ));
            }
            let child = slot.child.as_mut().expect("spawned above");
            // Never cancellable: the sidecar cannot interrupt a load, and
            // giving up on it would only throw away the model it is loading.
            match child
                .request(model.load_header(), &[], LOAD_TIMEOUT, None, &mut |_| {})
                .await
            {
                Ok(_) => {
                    slot.loaded = true;
                    self.hot.store(true, Ordering::Release);
                    self.books.lock().unwrap().model_rejected = false;
                }
                Err(failure) => return Err(self.handle_failure(&mut slot, failure).await),
            }
            self.touch();
        }
        if request.header.is_empty() {
            self.touch();
            return Ok(sidecar::Reply {
                header: serde_json::Value::Null,
                body: Vec::new(),
            });
        }
        if cancelled() {
            // The load kept the sidecar hot for the next request; this one
            // was given up on while it waited.
            self.touch();
            return Err(VoiceError::cancelled());
        }
        let child = slot.child.as_mut().expect("spawned above");
        let outcome = child
            .request(
                request.header,
                &request.body,
                request.bound,
                request
                    .cancellable
                    .then_some(&cancelled as &(dyn Fn() -> bool + Sync)),
                on_progress,
            )
            .await;
        self.touch();
        match outcome {
            Ok(reply) => {
                if request.invalidates_model {
                    // The files under the loaded model changed: the next
                    // request loads them afresh rather than trusting memory.
                    slot.loaded = false;
                    self.hot.store(false, Ordering::Release);
                }
                Ok(reply)
            }
            Err(failure) => Err(self.handle_failure(&mut slot, failure).await),
        }
    }

    fn spawn_into(self: &Arc<Self>, slot: &mut Slot) -> Result<(), VoiceError> {
        if self.crashed_too_often() {
            return Err(VoiceError::new(
                "voice_sidecar_failed",
                format!(
                    "the voice sidecar crashed {CRASH_LIMIT} times in the last minute; see {}",
                    sidecar::log_path(&self.cache_dir).display()
                ),
                false,
            ));
        }
        let uv = (self.uv_lookup)().map_err(|error| {
            VoiceError::new("voice_uv_missing", uv::install_hint(&error), false)
        })?;
        let spawned = crate::paths::prepare_voice_cache_dir(&self.cache_dir)
            .map_err(|error| error.to_string())
            .and_then(|()| sidecar::script_path(&self.cache_dir).map_err(|error| error.to_string()))
            .and_then(|script| {
                let command = (self.command_factory)(&uv, &script, &self.cache_dir);
                SidecarChild::spawn_with(command, &sidecar::log_path(&self.cache_dir))
                    .map_err(|error| error.to_string())
            });
        match spawned {
            Ok(child) => {
                self.child_pid.store(
                    child
                        .id()
                        .and_then(|pid| i32::try_from(pid).ok())
                        .unwrap_or(0),
                    Ordering::Release,
                );
                slot.child = Some(child);
                slot.loaded = false;
                self.start_idle_task();
                Ok(())
            }
            Err(detail) => {
                self.record_crash(format!("spawn failed: {detail}"));
                Err(self.sidecar_failed())
            }
        }
    }

    async fn handle_failure(&self, slot: &mut Slot, failure: SidecarFailure) -> VoiceError {
        match failure {
            SidecarFailure::Cancelled => VoiceError::cancelled(),
            SidecarFailure::CancelUnacknowledged => {
                // Stuck in work it cannot interrupt (bz2 extraction, a stalled
                // read): killed at the cancel's request, not counted against it.
                self.drop_child(slot).await;
                VoiceError::cancelled()
            }
            SidecarFailure::Refused { class, error } => {
                let error = bounded_detail(&error);
                self.books.lock().unwrap().last_error = error.clone();
                match class.as_str() {
                    "network" => VoiceError::new("voice_network_unavailable", error, true),
                    "input" => VoiceError::invalid(error),
                    "model" => {
                        // STATUS now reports MODEL_MISSING with this reason, so
                        // the phone can offer the re-provision that repairs it.
                        self.books.lock().unwrap().model_rejected = true;
                        VoiceError {
                            model_fault: true,
                            ..VoiceError::new("voice_sidecar_failed", error, false)
                        }
                    }
                    _ => VoiceError::new("voice_sidecar_failed", error, true),
                }
            }
            SidecarFailure::Timeout => {
                self.drop_child(slot).await;
                // A sidecar that keeps hanging is as broken as one that keeps
                // dying: it counts toward the same backoff.
                self.record_crash("sidecar timed out".into());
                VoiceError::new(
                    "voice_sidecar_timeout",
                    format!(
                        "the voice sidecar did not answer in time; see {}",
                        sidecar::log_path(&self.cache_dir).display()
                    ),
                    !self.crashed_too_often(),
                )
            }
            SidecarFailure::Crashed(detail) => {
                self.drop_child(slot).await;
                self.record_crash(bounded_detail(&detail));
                self.sidecar_failed()
            }
        }
    }

    async fn drop_child(&self, slot: &mut Slot) {
        self.hot.store(false, Ordering::Release);
        self.child_pid.store(0, Ordering::Release);
        slot.loaded = false;
        if let Some(child) = slot.child.take() {
            child.kill().await;
        }
    }

    fn record_crash(&self, detail: String) {
        let mut books = self.books.lock().unwrap();
        let now = Instant::now();
        books.crashes.push_back(now);
        while books
            .crashes
            .front()
            .is_some_and(|at| now.duration_since(*at) > CRASH_WINDOW)
        {
            books.crashes.pop_front();
        }
        books.last_error = detail;
    }

    fn crashed_too_often(&self) -> bool {
        let now = Instant::now();
        self.books
            .lock()
            .unwrap()
            .crashes
            .iter()
            .filter(|at| now.duration_since(**at) <= CRASH_WINDOW)
            .count()
            >= CRASH_LIMIT
    }

    fn sidecar_failed(&self) -> VoiceError {
        // Computed before the books are locked: `crashed_too_often` locks them
        // too, and `std::sync::Mutex` is not reentrant.
        let retryable = !self.crashed_too_often();
        let last_error = self.books.lock().unwrap().last_error.clone();
        VoiceError::new(
            "voice_sidecar_failed",
            format!(
                "{last_error}; see {}",
                sidecar::log_path(&self.cache_dir).display()
            ),
            retryable,
        )
    }

    fn touch(&self) {
        self.books.lock().unwrap().last_used = Some(Instant::now());
    }

    /// Unloads the sidecar once it has sat idle for `idle_after`; respawn is
    /// lazy. Started with the first spawn, never before.
    fn start_idle_task(self: &Arc<Self>) {
        {
            let mut books = self.books.lock().unwrap();
            if books.idle_task_started {
                return;
            }
            books.idle_task_started = true;
        }
        let weak = Arc::downgrade(self);
        let interval = self
            .idle_after
            .min(Duration::from_secs(30))
            .max(Duration::from_millis(10));
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                let Some(service) = weak.upgrade() else {
                    return;
                };
                service.unload_if_idle().await;
            }
        });
    }

    async fn unload_if_idle(&self) {
        let Ok(mut slot) = self.slot.try_lock() else {
            return;
        };
        if slot.child.is_none() {
            return;
        }
        let idle = self
            .books
            .lock()
            .unwrap()
            .last_used
            .is_none_or(|last| last.elapsed() >= self.idle_after);
        if !idle {
            return;
        }
        self.drop_child(&mut slot).await;
    }

    #[cfg(test)]
    async fn sidecar_pid(&self) -> Option<u32> {
        self.slot
            .lock()
            .await
            .child
            .as_ref()
            .and_then(SidecarChild::id)
    }

    /// Kills the sidecar. Called on cooperative daemon shutdown.
    ///
    /// Does not wait for the slot: a request in flight — a two-hour provision,
    /// a cold load — must not hold up `daemon-stop`. The child's process group
    /// is signalled directly; the request then fails and drops the child.
    pub(crate) async fn shutdown(&self) {
        match self.slot.try_lock() {
            Ok(mut slot) => self.drop_child(&mut slot).await,
            Err(_) => {
                let pid = self.child_pid.swap(0, Ordering::AcqRel);
                if pid > 0 {
                    // SAFETY: the pid names a process group this service
                    // spawned; a group already gone is an ESRCH, nothing more.
                    unsafe {
                        libc::kill(-pid, libc::SIGTERM);
                    }
                }
                self.hot.store(false, Ordering::Release);
            }
        }
    }
}

fn ping_header() -> serde_json::Map<String, serde_json::Value> {
    let mut header = serde_json::Map::new();
    header.insert("op".into(), "ping".into());
    header
}

struct SidecarRequest {
    header: serde_json::Map<String, serde_json::Value>,
    body: Vec<u8>,
    bound: Duration,
    needs_model: bool,
    /// Whether the sidecar can interrupt this op when told to. Only a
    /// provision can; sending `cancel` for anything else would only make the
    /// host give up on a healthy sidecar mid-load and pay a cold start.
    cancellable: bool,
    /// The op rewrites the model files, so a model held in memory is stale
    /// once it succeeds.
    invalidates_model: bool,
}

struct WaiterGuard<'a>(&'a AtomicUsize);

impl Drop for WaiterGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Sidecar diagnostics as `VoiceStatus.detail`: long enough to read, short
/// enough that a stray traceback cannot become the status payload.
fn bounded_detail(detail: &str) -> String {
    const MAX_DETAIL_CHARS: usize = 512;
    if detail.chars().count() <= MAX_DETAIL_CHARS {
        return detail.to_owned();
    }
    let mut cut: String = detail.chars().take(MAX_DETAIL_CHARS).collect();
    cut.push('…');
    cut
}

fn prune_sessions(sessions: &mut HashMap<String, Session>) {
    sessions.retain(|_, session| {
        session.since.elapsed() < SESSION_TTL && control_sink_alive(session.connection_id)
    });
}

/// BCP-47 shape: 2–3 letter language, then `-` separated 1–8 alphanumeric
/// subtags.
fn valid_language_tag(tag: &str) -> bool {
    let mut parts = tag.split('-');
    let language = parts.next().unwrap_or_default();
    (2..=3).contains(&language.len())
        && language.chars().all(|c| c.is_ascii_alphabetic())
        && parts.all(|part| {
            (1..=8).contains(&part.len()) && part.chars().all(|c| c.is_ascii_alphanumeric())
        })
}

/// Edge voice identifiers look like `en-US-AvaNeural` or
/// `en-US-AvaMultilingualNeural`.
fn valid_voice_id(voice: &str) -> bool {
    (8..=64).contains(&voice.len())
        && voice.ends_with("Neural")
        && voice
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
        && voice.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && voice.matches('-').count() >= 2
}

/// Ingest's entry point (§4.5): a `Stop` that landed the agent in Idle with a
/// message. Nothing happens unless a voice request has created the service
/// and a session exists for the agent.
pub(crate) fn on_agent_reply(reply: AgentReply) {
    if let Some(service) = VoiceService::existing() {
        service.push_reply(reply);
    }
}

/// Cooperative daemon shutdown: kill a hot sidecar rather than let
/// `kill_on_drop` race the runtime's teardown.
pub(crate) async fn shutdown() {
    if let Some(service) = VoiceService::existing() {
        service.shutdown().await;
    }
}

#[cfg(test)]
mod tests;
