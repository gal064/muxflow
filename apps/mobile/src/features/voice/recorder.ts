// The microphone side of voice mode (docs/mobile/voice-mode-plan.md §5.2).
// `VoiceRecorder` is what the controller drives; `createExpoRecorder` is the
// expo-audio implementation, imported only by the Voice screen so nothing
// audio-related loads until it opens (§2c).

import { AudioModule, AudioQuality, IOSOutputFormat, requestRecordingPermissionsAsync, setAudioModeAsync, type RecordingOptions } from "expo-audio";
import type { AudioRecorder } from "expo-audio";
import { Directory, File, Paths } from "expo-file-system";
import { Platform } from "react-native";
import { isBluetoothInput, playbackInputAfterBluetooth, preferredExternalInput } from "./inputRouting";
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

export function createExpoRecorder(log?: (line: string) => void): VoiceRecorder {
  let recorder: AudioRecorder | undefined;
  let prepared = false;
  let preparing: Promise<void> | undefined;
  let recording = false;
  let stopping = false;
  let startedAt = 0;
  // expo-audio creates the output file at prepare time (Android mints
  // `cache/Audio/recording-<uuid>.m4a` per prepare). A prepared recorder that
  // never records would leave that empty file behind, so the adapter tracks it.
  let preparedUri: string | null = null;
  let bluetoothRouteSelected = false;
  let swept = false;
  const state = (): "unprepared" | "preparing" | "prepared" | "recording" | "stopping" =>
    stopping ? "stopping" : recording ? "recording" : prepared ? "prepared" : preparing ? "preparing" : "unprepared";
  const report = (event: string): void => log?.(`voice.recorder ${event} actual=${state()}`);
  const dropPreparedFile = (): void => {
    if (preparedUri && !recording) deleteQuietly(preparedUri);
    preparedUri = null;
  };
  const prepareNow = async (): Promise<void> => {
    report("permission.requested");
    const permission = await requestRecordingPermissionsAsync();
    report(`permission.${permission.granted ? "granted" : "denied"}`);
    if (!permission.granted) throw new RecordingPermissionDenied();
    await setAudioModeAsync({
      allowsRecording: true,
      playsInSilentMode: true,
      interruptionMode: "duckOthers",
      // The loudspeaker remains the phone fallback, while selecting a headset
      // input below moves the play-and-record route to that headset as a pair.
      shouldRouteThroughEarpiece: false,
    });
    if (!swept) {
      // Leftovers from an earlier run (a crash mid-utterance): nothing is prepared yet, so all of them are stale.
      swept = true;
      sweepRecordings();
    }
    recorder ??= new AudioModule.AudioRecorder(nativeRecordingOptions(RECORDING_PRESET, Platform.OS));
    await recorder.prepareToRecordAsync();
    bluetoothRouteSelected = routeToPreferredInput(recorder);
    prepared = true;
    preparedUri = recorder.uri;
    report("prepared");
  };
  return {
    prepare() {
      if (prepared) return Promise.resolve();
      report("prepare.requested");
      // Two callers (a release re-arm and a re-focus) share one native prepare.
      preparing ??= prepareNow().finally(() => { preparing = undefined; });
      return preparing;
    },
    record() {
      if (!recorder || !prepared) throw new Error("recorder not prepared");
      bluetoothRouteSelected = routeToPreferredInput(recorder) || bluetoothRouteSelected;
      startedAt = Date.now();
      recorder.record();
      recording = true;
      report("started");
    },
    async stop() {
      // A release before `record()` ran (the press landed mid re-arm) has nothing to stop.
      if (!recorder || !recording) {
        report("stop.skipped");
        return { uri: null, durationMs: 0 };
      }
      recording = false;
      stopping = true;
      const durationMs = Math.max(0, Date.now() - startedAt);
      try {
        await recorder.stop();
      } finally {
        // A stopped (or failed) recorder must be prepared again before the next `record()`.
        prepared = false;
        stopping = false;
        restorePlaybackRoute(recorder, bluetoothRouteSelected);
        bluetoothRouteSelected = false;
        await setPlaybackAudioMode().catch(() => {});
        report(`stopped durationMs=${durationMs}`);
      }
      preparedUri = null;
      return { uri: recorder.uri, durationMs };
    },
    release() {
      if (recording) {
        report("release.skipped");
        return; // a live utterance is the controller's to stop
      }
      dropPreparedFile();
      restorePlaybackRoute(recorder, bluetoothRouteSelected);
      bluetoothRouteSelected = false;
      recorder?.release();
      recorder = undefined;
      prepared = false;
      void setPlaybackAudioMode().catch(() => {});
      report("released");
    },
    state,
  };
}

/**
 * Prefer the microphone that belongs to connected headphones. Expo exposes
 * this only after prepare. Selection happens at prepare time so Android's
 * asynchronous Bluetooth SCO route can settle, then again at press time in
 * case headphones connected while the screen was already open.
 *
 * Android 8/9 can enumerate inputs but cannot set one through MediaRecorder;
 * a routing refusal there must not turn a usable phone microphone into a
 * broken voice screen.
 */
function routeToPreferredInput(recorder: AudioRecorder): boolean {
  try {
    const input = preferredExternalInput(recorder.getAvailableInputs());
    if (input) {
      recorder.setInput(input.uid);
      return isBluetoothInput(input);
    }
  } catch {
    // Keep the system-selected route when this OS/device cannot override it.
  }
  return false;
}

function restorePlaybackRoute(recorder: AudioRecorder | undefined, bluetoothSelected: boolean): void {
  if (!recorder || !bluetoothSelected || Platform.OS !== "android") return;
  try {
    const fallback = playbackInputAfterBluetooth(recorder.getAvailableInputs());
    if (fallback) recorder.setInput(fallback.uid);
  } catch {
    // Playback still works through the OS-selected route if restoration is unsupported.
  }
}

function setPlaybackAudioMode(): Promise<void> {
  return setAudioModeAsync({
    allowsRecording: false,
    playsInSilentMode: true,
    interruptionMode: "duckOthers",
    shouldRouteThroughEarpiece: false,
  });
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
