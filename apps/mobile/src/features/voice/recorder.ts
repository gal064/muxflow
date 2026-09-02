// The microphone side of voice mode (docs/mobile/voice-mode-plan.md §5.2).
// `VoiceRecorder` is what the controller drives; `createExpoRecorder` is the
// expo-audio implementation, imported only by the Voice screen so nothing
// audio-related loads until it opens (§2c).

import { AudioModule, AudioQuality, IOSOutputFormat, requestRecordingPermissionsAsync, setAudioModeAsync, type RecordingOptions } from "expo-audio";
import type { AudioRecorder } from "expo-audio";
import type { VoiceRecorder } from "./audioPorts";

export type { VoiceRecorder } from "./audioPorts";
export { RECORDING_MIME } from "./audioPorts";

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

export class RecordingPermissionDenied extends Error {
  constructor() {
    super("Microphone permission was denied.");
    this.name = "RecordingPermissionDenied";
  }
}

export function createExpoRecorder(): VoiceRecorder {
  let recorder: AudioRecorder | undefined;
  let prepared = false;
  let preparing: Promise<void> | undefined;
  let recording = false;
  let startedAt = 0;
  const prepareNow = async (): Promise<void> => {
    const permission = await requestRecordingPermissionsAsync();
    if (!permission.granted) throw new RecordingPermissionDenied();
    await setAudioModeAsync({ allowsRecording: true, playsInSilentMode: true, interruptionMode: "duckOthers" });
    recorder ??= new AudioModule.AudioRecorder(RECORDING_PRESET);
    await recorder.prepareToRecordAsync();
    prepared = true;
  };
  return {
    prepare() {
      if (prepared) return Promise.resolve();
      // Two callers (a release re-arm and a re-focus) share one native prepare.
      preparing ??= prepareNow().finally(() => { preparing = undefined; });
      return preparing;
    },
    record() {
      if (!recorder || !prepared) throw new Error("recorder not prepared");
      startedAt = Date.now();
      recorder.record();
      recording = true;
    },
    async stop() {
      // A release before `record()` ran (the press landed mid re-arm) has nothing to stop.
      if (!recorder || !recording) return { uri: null, durationMs: 0 };
      recording = false;
      const durationMs = Math.max(0, Date.now() - startedAt);
      try {
        await recorder.stop();
      } finally {
        // A stopped (or failed) recorder must be prepared again before the next `record()`.
        prepared = false;
      }
      return { uri: recorder.uri, durationMs };
    },
    release() {
      recorder?.release();
      recorder = undefined;
      prepared = false;
      recording = false;
    },
  };
}
