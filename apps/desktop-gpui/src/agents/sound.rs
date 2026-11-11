//! The agent cues: a short synthesized tone per event, no media file. Port of
//! `sound.ts`, with rodio standing in for Web Audio.
//!
//! The Tauri app's one-time import from `localStorage` is not carried over:
//! this app has no WebView storage to read, and app state has held the value
//! since that import shipped.

use std::num::NonZero;

use desktop_core::app_state::{AgentSoundMode, AgentSoundPreferences};

use super::model::AttentionKind;

pub const DEFAULT_PREFERENCES: AgentSoundPreferences = AgentSoundPreferences {
    enabled: true,
    blocked: AgentSoundMode::Subtle,
    completed: AgentSoundMode::Subtle,
    volume: 0.45,
};

/// A saved value made safe to use: an out-of-range volume is clamped, a
/// non-finite one falls back. Nothing saved is the defaults.
pub fn effective(saved: Option<AgentSoundPreferences>) -> AgentSoundPreferences {
    let Some(saved) = saved else {
        return DEFAULT_PREFERENCES;
    };
    AgentSoundPreferences {
        volume: if saved.volume.is_finite() {
            saved.volume.clamp(0., 1.)
        } else {
            DEFAULT_PREFERENCES.volume
        },
        ..saved
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoundOutcome {
    Played,
    Disabled,
    Failed,
}

pub fn enabled_for(event: AttentionKind, preferences: &AgentSoundPreferences) -> bool {
    let mode = match event {
        AttentionKind::Blocked => preferences.blocked,
        AttentionKind::Completed => preferences.completed,
    };
    preferences.enabled && mode != AgentSoundMode::None && preferences.volume > 0.
}

const SAMPLE_RATE: u32 = 48_000;
const FLOOR: f64 = 0.0001;
const ATTACK_SECONDS: f64 = 0.012;
const RELEASE_AT_SECONDS: f64 = 0.16;
const STOP_SECONDS: f64 = 0.17;

/// The cue's samples, mono: a sine at 520 Hz for blocked and 660 Hz for done,
/// rising exponentially to `volume * 0.12` in 12 ms and back to silence by
/// 160 ms, as the Web Audio graph did.
pub fn synthesize(event: AttentionKind, volume: f64) -> Vec<f32> {
    let frequency = match event {
        AttentionKind::Blocked => 520.,
        AttentionKind::Completed => 660.,
    };
    let peak = (volume * 0.12).max(FLOOR);
    let ramp = |from: f64, to: f64, progress: f64| from * (to / from).powf(progress);
    let total = (STOP_SECONDS * f64::from(SAMPLE_RATE)) as usize;
    (0..total)
        .map(|index| {
            let time = index as f64 / f64::from(SAMPLE_RATE);
            let gain = if time < ATTACK_SECONDS {
                ramp(FLOOR, peak, time / ATTACK_SECONDS)
            } else if time < RELEASE_AT_SECONDS {
                ramp(
                    peak,
                    FLOOR,
                    (time - ATTACK_SECONDS) / (RELEASE_AT_SECONDS - ATTACK_SECONDS),
                )
            } else {
                FLOOR
            };
            (gain * (std::f64::consts::TAU * frequency * time).sin()) as f32
        })
        .collect()
}

/// Plays the cue on its own thread, which owns the output stream until the
/// tone ends. Errors are logged: a missing device is no reason to interrupt.
pub fn play(event: AttentionKind, preferences: AgentSoundPreferences) -> SoundOutcome {
    if !enabled_for(event, &preferences) {
        return SoundOutcome::Disabled;
    }
    let samples = synthesize(event, preferences.volume);
    let spawned = std::thread::Builder::new()
        .name("agent-sound".into())
        .spawn(move || {
            let result = rodio::DeviceSinkBuilder::open_default_sink().map(|mut sink| {
                sink.log_on_drop(false);
                let player = rodio::Player::connect_new(sink.mixer());
                player.append(rodio::buffer::SamplesBuffer::new(
                    NonZero::<u16>::MIN,
                    NonZero::new(SAMPLE_RATE).expect("nonzero rate"),
                    samples,
                ));
                player.sleep_until_end();
            });
            if let Err(error) = result {
                eprintln!("agent sound unavailable: {error}");
            }
        });
    match spawned {
        Ok(_) => SoundOutcome::Played,
        Err(_) => SoundOutcome::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_out_of_range_saved_values_and_defaults_nothing_saved() {
        let saved = AgentSoundPreferences {
            enabled: true,
            blocked: AgentSoundMode::Subtle,
            completed: AgentSoundMode::None,
            volume: 7.,
        };
        assert_eq!(effective(Some(saved)).volume, 1.);
        let nan = AgentSoundPreferences {
            enabled: false,
            volume: f64::NAN,
            ..DEFAULT_PREFERENCES
        };
        assert_eq!(
            effective(Some(nan)),
            AgentSoundPreferences {
                enabled: false,
                ..DEFAULT_PREFERENCES
            }
        );
        assert_eq!(effective(None), DEFAULT_PREFERENCES);
    }

    #[test]
    fn reports_disabled_playback_without_touching_an_audio_device() {
        let off = AgentSoundPreferences {
            enabled: false,
            ..DEFAULT_PREFERENCES
        };
        assert_eq!(play(AttentionKind::Blocked, off), SoundOutcome::Disabled);
        let muted = AgentSoundPreferences {
            blocked: AgentSoundMode::None,
            ..DEFAULT_PREFERENCES
        };
        assert_eq!(play(AttentionKind::Blocked, muted), SoundOutcome::Disabled);
        assert!(enabled_for(AttentionKind::Completed, &muted));
        let silent = AgentSoundPreferences {
            volume: 0.,
            ..DEFAULT_PREFERENCES
        };
        assert!(!enabled_for(AttentionKind::Completed, &silent));
    }

    #[test]
    fn synthesizes_a_short_quiet_enveloped_tone() {
        let samples = synthesize(AttentionKind::Blocked, 0.45);
        assert_eq!(samples.len(), (0.17 * 48_000.) as usize);
        let peak = samples
            .iter()
            .fold(0f32, |peak, sample| peak.max(sample.abs()));
        assert!(peak <= (0.45 * 0.12) as f32 + 1e-6 && peak > 0.04, "{peak}");
        // Silent at both ends, as the exponential ramps start and end at the floor.
        assert!(samples[0].abs() < 1e-3);
        assert!(samples.last().unwrap().abs() < 1e-3);
        assert_ne!(samples, synthesize(AttentionKind::Completed, 0.45));
    }
}
