import type { RecordingOptions } from "expo-audio";
import { describe, expect, it } from "vitest";

import { nativeRecordingOptions } from "./recordingOptions";

const preset = {
  extension: ".m4a",
  sampleRate: 16_000,
  numberOfChannels: 1,
  bitRate: 48_000,
  android: { outputFormat: "mpeg4", audioEncoder: "aac" },
  ios: { outputFormat: "aac ", audioQuality: 64 },
  web: {},
} as unknown as RecordingOptions;

describe("nativeRecordingOptions", () => {
  it("lifts the Android block to the top level, where the native constructor reads it", () => {
    const flat = nativeRecordingOptions(preset, "android") as Record<string, unknown>;
    expect(flat).toMatchObject({ extension: ".m4a", sampleRate: 16_000, numberOfChannels: 1, bitRate: 48_000, outputFormat: "mpeg4", audioEncoder: "aac" });
    expect(flat.audioQuality).toBeUndefined();
  });

  it("lifts the iOS block on iOS", () => {
    const flat = nativeRecordingOptions(preset, "ios") as Record<string, unknown>;
    expect(flat).toMatchObject({ outputFormat: "aac ", audioQuality: 64, sampleRate: 16_000 });
    expect(flat.audioEncoder).toBeUndefined();
  });
});
