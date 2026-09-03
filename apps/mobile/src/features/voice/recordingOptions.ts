import type { RecordingOptions } from "expo-audio";

/**
 * The native `AudioRecorder` takes one flat options record; the per-platform
 * `android`/`ios` blocks of `RecordingOptions` are flattened by expo-audio's
 * `useAudioRecorder` hook, not by the constructor. Constructing the native
 * class directly (as the recorder adapter does, to keep it out of React state)
 * means flattening here, or Android silently falls back to `MediaRecorder`'s
 * defaults: AMR-NB at 8 kHz in a 3GP container, which the host cannot decode
 * ("no audio track" on every utterance).
 */
export function nativeRecordingOptions(preset: RecordingOptions, os: string): RecordingOptions {
  const platform = os === "ios" ? preset.ios : preset.android;
  return { ...preset, ...platform };
}
