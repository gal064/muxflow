//! `muxflow-host voice …`: the voice service driven from a shell, for QA on
//! a host without a phone (docs/mobile/voice-mode-plan.md §4.1).
//!
//! `MUXFLOW_VOICE_CACHE_DIR` isolates a run; `MUXFLOW_VOICE_IDLE_SECS` and
//! `--linger SECS` together let the idle unload be watched in `ps`.

use std::{
    path::PathBuf,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use tmux_agent_protocol::v1;

use crate::service::voice::VoiceService;

const USAGE: &str = "usage: muxflow-host voice <status|provision --yes|transcribe FILE [--mime TYPE]|speak TEXT --out FILE [--voice ID]> [--linger SECS]";

pub(crate) async fn run(arguments: Vec<String>) -> anyhow::Result<()> {
    let verb = arguments.first().map(String::as_str);
    let flag = |name: &str| -> Option<String> {
        arguments
            .iter()
            .position(|argument| argument == name)
            .and_then(|index| arguments.get(index + 1).cloned())
    };
    let has = |name: &str| arguments.iter().any(|argument| argument == name);
    let positional: Vec<&String> = arguments
        .iter()
        .skip(1)
        .enumerate()
        .filter(|(index, argument)| {
            !argument.starts_with("--")
                && !arguments.get(*index).is_some_and(|previous| {
                    matches!(
                        previous.as_str(),
                        "--mime" | "--out" | "--voice" | "--linger"
                    )
                })
        })
        .map(|(_, argument)| argument)
        .collect();
    let service = VoiceService::global();
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    match verb {
        Some("status") => {
            println!("{}", status_json(&service.status()));
        }
        Some("provision") => {
            if !has("--yes") {
                bail!(
                    "provision downloads ~{} MB of speech model into {}; re-run with --yes to consent",
                    crate::service::voice::provision::MODEL_DOWNLOAD_BYTES / 1_000_000,
                    crate::paths::voice_cache_dir().display()
                );
            }
            let mut last_phase = String::new();
            let mut last_print = Instant::now() - Duration::from_secs(1);
            let mut on_progress = |progress: &v1::VoiceProvisionProgress| {
                let phase_changed = progress.phase != last_phase;
                if phase_changed || last_print.elapsed() >= Duration::from_millis(500) {
                    eprintln!(
                        "{:>7.1}s {:<19} {}/{} bytes{}",
                        started.elapsed().as_secs_f64(),
                        progress.phase,
                        progress.transferred_bytes,
                        progress.total_bytes,
                        if progress.error.is_empty() {
                            String::new()
                        } else {
                            format!("  error: {}", progress.error)
                        }
                    );
                    last_phase = progress.phase.clone();
                    last_print = Instant::now();
                }
            };
            let status = service
                .provision("cli", true, &cancel, None, &mut on_progress)
                .await
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            println!("{}", status_json(&status));
            eprintln!("provisioned in {:.1}s", started.elapsed().as_secs_f64());
        }
        Some("transcribe") => {
            let file = positional.first().map(PathBuf::from).context(USAGE)?;
            let mime = flag("--mime").unwrap_or_else(|| "audio/mp4".into());
            let audio = std::fs::read(&file).with_context(|| format!("read {}", file.display()))?;
            let transcript = service
                .transcribe(audio, &mime, "", &cancel)
                .await
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            println!(
                "{}",
                serde_json::json!({
                    "text": transcript.text,
                    "audioMillis": transcript.audio_millis,
                    "decodeMillis": transcript.decode_millis,
                    "roundTripMillis": started.elapsed().as_millis() as u64,
                })
            );
        }
        Some("speak") => {
            let text = positional.first().context(USAGE)?;
            let out = flag("--out").map(PathBuf::from).context(USAGE)?;
            let voice = flag("--voice").unwrap_or_default();
            let speech = service
                .speak(text, v1::VoiceProvider::EdgeTts, &voice, &cancel)
                .await
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            std::fs::write(&out, &speech.audio)
                .with_context(|| format!("write {}", out.display()))?;
            println!(
                "{}",
                serde_json::json!({
                    "bytes": speech.audio.len(),
                    "mime": speech.audio_mime,
                    "displayMarkdown": speech.display_markdown,
                    "speechText": speech.speech_text,
                    "truncated": speech.truncated,
                    "voice": speech.voice,
                    "out": out,
                    "roundTripMillis": started.elapsed().as_millis() as u64,
                })
            );
        }
        _ => bail!(USAGE),
    }
    if let Some(linger) = flag("--linger") {
        let seconds: u64 = linger.parse().context("--linger takes whole seconds")?;
        eprintln!(
            "lingering {seconds}s so the idle unload can be observed (pid {})",
            std::process::id()
        );
        tokio::time::sleep(Duration::from_secs(seconds)).await;
    }
    service.shutdown().await;
    Ok(())
}

fn status_json(status: &v1::VoiceStatus) -> serde_json::Value {
    serde_json::json!({
        "readiness": v1::VoiceReadiness::try_from(status.readiness)
            .map(|readiness| readiness.as_str_name())
            .unwrap_or("UNKNOWN"),
        "uvPath": status.uv_path,
        "modelDir": status.model_dir,
        "modelDownloadBytes": status.model_download_bytes,
        "detail": status.detail,
        "sidecarRunning": status.sidecar_running,
        "provision": status.provision.as_ref().map(|progress| serde_json::json!({
            "phase": progress.phase,
            "transferredBytes": progress.transferred_bytes,
            "totalBytes": progress.total_bytes,
        })),
    })
}
