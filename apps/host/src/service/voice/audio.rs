//! Utterance decoding (docs/mobile/voice-mode-plan.md §4.4).
//!
//! The phone records AAC in an MP4/M4A container. Decoding here, in Rust, keeps
//! ffmpeg and PyAV out of the sidecar. The model accepts only mono 16 kHz f32,
//! so this layer also resamples clips whose container rate differs.

use std::io::Cursor;

use rubato::{FftFixedIn, Resampler};

use symphonia::core::{
    audio::{Audio, GenericAudioBufferRef},
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
const MODEL_SAMPLE_RATE: u32 = 16_000;
const RESAMPLE_CHUNK: usize = 1024;
const MIN_SUPPORTED_SAMPLE_RATE: u32 = 7_350;
const MAX_SUPPORTED_SAMPLE_RATE: u32 = 192_000;

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

/// Decodes the whole utterance to mono 16 kHz f32.
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
        // The AAC decoder always yields f32 planes. Taking that variant and
        // averaging the planes directly, rather than through symphonia's
        // generic interleave-and-convert path, keeps the per-sample-format
        // conversion matrix out of the binary (docs/mobile/voice-mode-plan.md
        // §2b size budget).
        let GenericAudioBufferRef::F32(buffer) = decoded else {
            return Err(AudioError::Undecodable(
                "decoder produced a non-f32 sample format".into(),
            ));
        };
        if pcm.sample_rate == 0 {
            pcm.sample_rate = buffer.spec().rate();
        }
        let planes: Vec<&[f32]> = (0..buffer.num_planes())
            .filter_map(|index| buffer.plane(index))
            .collect();
        if planes.is_empty() {
            continue;
        }
        let scale = 1.0 / planes.len() as f32;
        pcm.samples.extend((0..buffer.frames()).map(|frame| {
            planes
                .iter()
                .map(|plane| plane.get(frame).copied().unwrap_or(0.0))
                .sum::<f32>()
                * scale
        }));
        if pcm.duration_millis() > MAX_AUDIO_MILLIS {
            return Err(AudioError::TooLong);
        }
    }
    if pcm.samples.is_empty() || pcm.sample_rate == 0 {
        return Err(AudioError::Undecodable("stream produced no samples".into()));
    }
    resample_for_model(pcm)
}

fn resample_for_model(mut pcm: PcmMono) -> Result<PcmMono, AudioError> {
    if !(MIN_SUPPORTED_SAMPLE_RATE..=MAX_SUPPORTED_SAMPLE_RATE).contains(&pcm.sample_rate) {
        return Err(AudioError::Undecodable(format!(
            "audio sample rate {} Hz is unsupported",
            pcm.sample_rate
        )));
    }
    if pcm.sample_rate == MODEL_SAMPLE_RATE {
        return Ok(pcm);
    }
    let input_rate = usize::try_from(pcm.sample_rate)
        .map_err(|_| AudioError::Undecodable("audio sample rate is unsupported".into()))?;
    let output_rate = MODEL_SAMPLE_RATE as usize;
    let input_len = pcm.samples.len();
    let expected_len = input_len.saturating_mul(output_rate).div_ceil(input_rate);
    let mut resampler = FftFixedIn::<f32>::new(input_rate, output_rate, RESAMPLE_CHUNK, 1, 1)
        .map_err(|error| AudioError::Undecodable(format!("resample setup: {error}")))?;
    let delay = resampler.output_delay();
    let wanted_with_delay = expected_len.saturating_add(delay);
    let mut output = Vec::with_capacity(wanted_with_delay);
    let (chunks, remainder) = pcm.samples.as_chunks::<RESAMPLE_CHUNK>();
    for chunk in chunks {
        let resampled = resampler
            .process(&[chunk.as_slice()], None)
            .map_err(|error| AudioError::Undecodable(format!("resample: {error}")))?;
        output.extend_from_slice(&resampled[0]);
    }
    if !remainder.is_empty() {
        let resampled = resampler
            .process_partial(Some(&[remainder]), None)
            .map_err(|error| AudioError::Undecodable(format!("resample tail: {error}")))?;
        output.extend_from_slice(&resampled[0]);
    }
    // At co-prime rates FftFixedIn buffers up to roughly one input-rate
    // worth of frames before producing its next output block. Each flush
    // contributes RESAMPLE_CHUNK zeros, so this rate-derived bound is enough
    // to expose the delay while remaining capped by MAX_SUPPORTED_SAMPLE_RATE.
    let max_flushes = input_rate.div_ceil(RESAMPLE_CHUNK).saturating_add(2);
    for _ in 0..max_flushes {
        if output.len() >= wanted_with_delay {
            break;
        }
        let resampled = resampler
            .process_partial::<&[f32]>(None, None)
            .map_err(|error| AudioError::Undecodable(format!("resample flush: {error}")))?;
        output.extend_from_slice(&resampled[0]);
    }
    if output.len() < wanted_with_delay {
        return Err(AudioError::Undecodable(
            "resampler did not flush the complete utterance".into(),
        ));
    }
    output.drain(..delay);
    output.truncate(expected_len);
    pcm.sample_rate = MODEL_SAMPLE_RATE;
    pcm.samples = output;
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

    #[test]
    fn resampling_normalizes_common_rates_and_preserves_duration_and_signal() {
        for input_rate in [7_350, 8_000, 44_100, 48_000, 192_000] {
            let samples = (0..input_rate)
                .map(|index| {
                    (2.0 * std::f32::consts::PI * 440.0 * index as f32 / input_rate as f32).sin()
                })
                .collect();
            let pcm = resample_for_model(PcmMono {
                sample_rate: input_rate,
                samples,
            })
            .unwrap();
            assert_eq!(pcm.sample_rate, MODEL_SAMPLE_RATE);
            assert_eq!(pcm.samples.len(), MODEL_SAMPLE_RATE as usize);
            assert!(pcm.samples.iter().any(|sample| sample.abs() > 0.5));
        }
    }

    #[test]
    fn resampling_rejects_rates_that_could_request_unbounded_fft_buffers() {
        for sample_rate in [0, 7_349, 192_001, u32::MAX] {
            let result = resample_for_model(PcmMono {
                sample_rate,
                samples: vec![0.0; 16],
            });
            assert!(
                matches!(result, Err(AudioError::Undecodable(detail)) if detail.contains("sample rate")),
                "rate {sample_rate} was not rejected"
            );
        }
    }

    #[test]
    fn resampling_flushes_short_audio_at_a_co_prime_supported_rate() {
        let input_rate = 191_999;
        let input_len = input_rate / 5;
        let pcm = resample_for_model(PcmMono {
            sample_rate: input_rate as u32,
            samples: vec![0.25; input_len],
        })
        .unwrap();
        assert_eq!(pcm.sample_rate, MODEL_SAMPLE_RATE);
        assert_eq!(pcm.samples.len(), MODEL_SAMPLE_RATE as usize / 5);
    }
}
