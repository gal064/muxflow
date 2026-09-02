//! Utterance decoding (docs/mobile/voice-mode-plan.md §4.4).
//!
//! The phone records AAC in an MP4/M4A container. Decoding here, in Rust, keeps
//! ffmpeg and PyAV out of the sidecar: the sidecar receives raw mono f32 PCM
//! and sherpa-onnx resamples it to the model's 16 kHz itself, so this only
//! downmixes.

use std::io::Cursor;

use symphonia::core::{
    codecs::{CodecParameters, audio::AudioDecoderOptions},
    errors::Error,
    formats::{FormatOptions, TrackType, probe::Hint},
    io::{MediaSourceStream, MediaSourceStreamOptions},
    meta::MetadataOptions,
};

/// Hard cap on one utterance payload; a 30 s mono 16 kHz AAC utterance is
/// ~200 KB, so this is forty times what the phone sends.
pub(crate) const MAX_AUDIO_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MIN_AUDIO_MILLIS: u32 = 200;
pub(crate) const MAX_AUDIO_MILLIS: u32 = 120_000;
const MAX_SKIPPED_PACKETS: u32 = 64;

const ACCEPTED_MIME_TYPES: [&str; 4] = ["audio/mp4", "audio/x-m4a", "audio/m4a", "audio/aac"];

pub(crate) fn accepted_mime(mime: &str) -> bool {
    let essence = mime.split(';').next().unwrap_or_default().trim();
    ACCEPTED_MIME_TYPES
        .iter()
        .any(|accepted| accepted.eq_ignore_ascii_case(essence))
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PcmMono {
    pub(crate) sample_rate: u32,
    pub(crate) samples: Vec<f32>,
}

impl PcmMono {
    pub(crate) fn duration_millis(&self) -> u32 {
        if self.sample_rate == 0 {
            return 0;
        }
        u32::try_from(self.samples.len() as u64 * 1000 / u64::from(self.sample_rate))
            .unwrap_or(u32::MAX)
    }
}

#[derive(Debug)]
pub(crate) enum AudioError {
    Undecodable(String),
    TooLong,
}

/// Decodes the whole utterance to mono f32 at the container's own rate.
///
/// Stops as soon as more than [`MAX_AUDIO_MILLIS`] have been decoded, so a
/// payload that is small on the wire but long in time cannot buy a minute of
/// decoding. Individual undecodable packets are skipped, as symphonia allows;
/// a stream that yields no samples at all is undecodable.
pub(crate) fn decode_to_mono_f32(bytes: Vec<u8>, mime: &str) -> Result<PcmMono, AudioError> {
    let source = MediaSourceStream::new(
        Box::new(Cursor::new(bytes)),
        MediaSourceStreamOptions::default(),
    );
    let mut hint = Hint::new();
    hint.mime_type(mime.split(';').next().unwrap_or_default().trim());
    hint.with_extension("m4a");
    let mut reader = symphonia::default::get_probe()
        .probe(
            &hint,
            source,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|error| AudioError::Undecodable(format!("container: {error}")))?;
    let track = reader
        .default_track(TrackType::Audio)
        .ok_or_else(|| AudioError::Undecodable("no audio track".into()))?;
    let Some(CodecParameters::Audio(parameters)) = &track.codec_params else {
        return Err(AudioError::Undecodable("no audio codec parameters".into()));
    };
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(parameters, &AudioDecoderOptions::default())
        .map_err(|error| AudioError::Undecodable(format!("codec: {error}")))?;
    let mut pcm = PcmMono {
        sample_rate: 0,
        samples: Vec::new(),
    };
    let mut interleaved = Vec::new();
    // Symphonia lets a caller skip a malformed packet and continue; a stream
    // that does nothing but produce them is not worth continuing on.
    let mut skipped = 0_u32;
    loop {
        if skipped > MAX_SKIPPED_PACKETS {
            return Err(AudioError::Undecodable("too many malformed packets".into()));
        }
        let packet = match reader.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(Error::DecodeError(_)) => {
                skipped += 1;
                continue;
            }
            Err(error) => return Err(AudioError::Undecodable(format!("demux: {error}"))),
        };
        if packet.track_id != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(Error::DecodeError(_)) => {
                skipped += 1;
                continue;
            }
            Err(error) => return Err(AudioError::Undecodable(format!("decode: {error}"))),
        };
        let spec = decoded.spec();
        let channels = spec.channels().count().max(1);
        if pcm.sample_rate == 0 {
            pcm.sample_rate = spec.rate();
        }
        interleaved.clear();
        decoded.copy_to_vec_interleaved::<f32>(&mut interleaved);
        pcm.samples.extend(
            interleaved
                .chunks(channels)
                .map(|frame| frame.iter().sum::<f32>() / channels as f32),
        );
        if pcm.duration_millis() > MAX_AUDIO_MILLIS {
            return Err(AudioError::TooLong);
        }
    }
    if pcm.samples.is_empty() || pcm.sample_rate == 0 {
        return Err(AudioError::Undecodable("stream produced no samples".into()));
    }
    Ok(pcm)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!("fixtures/hello.m4a");

    #[test]
    fn the_fixture_decodes_to_about_one_second_of_mono_samples() {
        assert!(FIXTURE.len() < 20 * 1024);
        let pcm = decode_to_mono_f32(FIXTURE.to_vec(), "audio/mp4").unwrap();
        assert_eq!(pcm.sample_rate, 16_000);
        let millis = pcm.duration_millis();
        assert!((900..=1_150).contains(&millis), "{millis} ms");
        let peak = pcm
            .samples
            .iter()
            .fold(0.0_f32, |peak, s| peak.max(s.abs()));
        assert!(peak > 0.1 && peak <= 1.0, "peak {peak}");
    }

    #[test]
    fn garbage_is_undecodable_and_the_mime_allowlist_is_exact() {
        assert!(matches!(
            decode_to_mono_f32(b"not audio at all".to_vec(), "audio/mp4"),
            Err(AudioError::Undecodable(_))
        ));
        assert!(matches!(
            decode_to_mono_f32(Vec::new(), "audio/mp4"),
            Err(AudioError::Undecodable(_))
        ));
        for accepted in [
            "audio/mp4",
            "audio/x-m4a",
            "AUDIO/AAC",
            "audio/mp4; codecs=mp4a",
        ] {
            assert!(accepted_mime(accepted), "{accepted}");
        }
        for refused in ["audio/mpeg", "audio/wav", "", "text/plain"] {
            assert!(!accepted_mime(refused), "{refused}");
        }
    }

    #[test]
    fn duration_is_derived_from_samples_and_rate() {
        let pcm = PcmMono {
            sample_rate: 16_000,
            samples: vec![0.0; 16_000 * 121],
        };
        assert_eq!(pcm.duration_millis(), 121_000);
        assert!(pcm.duration_millis() > MAX_AUDIO_MILLIS);
    }
}
