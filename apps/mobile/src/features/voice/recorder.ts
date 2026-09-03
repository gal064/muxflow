// The microphone side of voice mode (docs/mobile/voice-mode-plan.md §5.2).
// `VoiceRecorder` is what the controller drives; `createExpoRecorder` is the
// expo-audio implementation, imported only by the Voice screen so nothing
// audio-related loads until it opens (§2c).

import { AudioModule, AudioQuality, IOSOutputFormat, requestRecordingPermissionsAsync, setAudioModeAsync, type RecordingOptions } from "expo-audio";
import type { AudioRecorder } from "expo-audio";
import { Directory, File, Paths } from "expo-file-system";
import { Platform } from "react-native";
import { nativeRecordingOptions } from "./recordingOptions";
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
  // expo-audio creates the output file at prepare time (Android mints
  // `cache/Audio/recording-<uuid>.m4a` per prepare). A prepared recorder that
  // never records would leave that empty file behind, so the adapter tracks it.
  let preparedUri: string | null = null;
  let swept = false;
  const dropPreparedFile = (): void => {
    if (preparedUri && !recording) deleteQuietly(preparedUri);
    preparedUri = null;
  };
  const prepareNow = async (): Promise<void> => {
    const permission = await requestRecordingPermissionsAsync();
    if (!permission.granted) throw new RecordingPermissionDenied();
    await setAudioModeAsync({ allowsRecording: true, playsInSilentMode: true, interruptionMode: "duckOthers" });
    if (!swept) {
      // Leftovers from an earlier run (a crash mid-utterance): nothing is prepared yet, so all of them are stale.
      swept = true;
      sweepRecordings();
    }
    recorder ??= new AudioModule.AudioRecorder(nativeRecordingOptions(RECORDING_PRESET, Platform.OS));
    await recorder.prepareToRecordAsync();
    prepared = true;
    preparedUri = recorder.uri;
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
      preparedUri = null;
      return { uri: recorder.uri, durationMs };
    },
    release() {
      if (recording) return; // a live utterance is the controller's to stop
      dropPreparedFile();
      recorder?.release();
      recorder = undefined;
      prepared = false;
    },
  };
}

function deleteQuietly(uri: string): void {
  try {
    const file = new File(uri);
    if (file.exists) file.delete();
  } catch {
    // Already gone is the outcome we wanted.
  }
}

/** Every `recording-*` file expo-audio left under the cache's `Audio/` directory. */
function sweepRecordings(): void {
  try {
    const directory = new Directory(Paths.cache, "Audio");
    if (!directory.exists) return;
    for (const entry of directory.list()) {
      if (entry instanceof File && entry.name.startsWith("recording-")) deleteQuietly(entry.uri);
    }
  } catch {
    // A cache the OS is clearing under us needs no sweeping.
  }
}
