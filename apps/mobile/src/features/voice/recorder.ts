// The microphone side of voice mode (docs/mobile/voice-mode-plan.md §5.2).
// `VoiceRecorder` is what the controller drives; `createExpoRecorder` is the
// expo-audio implementation, imported only by the Voice screen so nothing
// audio-related loads until it opens (§2c).

import { AudioModule, AudioQuality, IOSOutputFormat, requestRecordingPermissionsAsync, setAudioModeAsync, type RecordingOptions } from "expo-audio";
import type { AudioRecorder } from "expo-audio";

export interface VoiceRecorder {
  /** Permission, audio mode and `prepareToRecordAsync`, so press-in only calls `record()` (§2b). */
  prepare(): Promise<void>;
  record(): void;
  /** Stops and returns the file and its length; `uri` is null when nothing was captured. */
  stop(): Promise<{ uri: string | null; durationMs: number }>;
  release(): void;
}

/** Mono 16 kHz AAC at 48 kbps in an `.m4a` container: ~6 KB/s, what parakeet wants (§2b). */
export const RECORDING_PRESET: RecordingOptions = {
  extension: ".m4a",
  sampleRate: 16_000,
  numberOfChannels: 1,
  bitRate: 48_000,
  android: { outputFormat: "mpeg4", audioEncoder: "aac" },
  ios: { outputFormat: IOSOutputFormat.MPEG4AAC, audioQuality: AudioQuality.MEDIUM },
  web: {},
};

/** `Request.voice.audioMime` for the preset above. */
export const RECORDING_MIME = "audio/mp4";

export class RecordingPermissionDenied extends Error {
  constructor() {
    super("Microphone permission was denied.");
    this.name = "RecordingPermissionDenied";
  }
}

export function createExpoRecorder(): VoiceRecorder {
  let recorder: AudioRecorder | undefined;
  let prepared = false;
  let startedAt = 0;
  return {
    async prepare() {
      if (prepared) return;
      const permission = await requestRecordingPermissionsAsync();
      if (!permission.granted) throw new RecordingPermissionDenied();
      await setAudioModeAsync({ allowsRecording: true, playsInSilentMode: true, interruptionMode: "duckOthers" });
      recorder ??= new AudioModule.AudioRecorder(RECORDING_PRESET);
      await recorder.prepareToRecordAsync();
      prepared = true;
    },
    record() {
      if (!recorder || !prepared) throw new Error("recorder not prepared");
      startedAt = Date.now();
      recorder.record();
    },
    async stop() {
      if (!recorder) return { uri: null, durationMs: 0 };
      const durationMs = Math.max(0, Date.now() - startedAt);
      await recorder.stop();
      // A stopped recorder must be prepared again before the next `record()`.
      prepared = false;
      return { uri: recorder.uri, durationMs };
    },
    release() {
      recorder?.release();
      recorder = undefined;
      prepared = false;
    },
  };
}
