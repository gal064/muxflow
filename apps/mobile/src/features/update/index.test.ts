import { beforeEach, describe, expect, it, vi } from "vitest";

const { platform, openURL } = vi.hoisted(() => ({
  platform: { OS: "ios" },
  openURL: vi.fn().mockResolvedValue(undefined),
}));
vi.mock("react-native", () => ({
  Platform: platform,
  Linking: { openURL },
  AppState: { currentState: "active", addEventListener: vi.fn() },
}));
vi.mock("expo-constants", () => ({ default: { expoConfig: { version: "0.1.9" } } }));

import { openAppUpdates, openDesktopUpdates } from "./index";

describe("distribution update destinations", () => {
  beforeEach(() => {
    platform.OS = "ios";
    openURL.mockReset().mockResolvedValue(undefined);
  });

  it("opens TestFlight for an iOS app update", async () => {
    await openAppUpdates();
    expect(openURL.mock.calls).toEqual([["itms-beta://"]]);
  });

  it("offers TestFlight installation when its scheme is unavailable", async () => {
    openURL.mockRejectedValueOnce(new Error("TestFlight is not installed"));
    await openAppUpdates();
    expect(openURL.mock.calls).toEqual([
      ["itms-beta://"],
      ["https://apps.apple.com/app/testflight/id899247664"],
    ]);
  });

  it("opens the desktop/helper release destination from an iPhone", async () => {
    await openDesktopUpdates();
    expect(openURL.mock.calls).toEqual([["https://github.com/gal064/muxflow/releases/latest"]]);
  });

  it("preserves the Android app's GitHub release destination", async () => {
    platform.OS = "android";
    await openAppUpdates();
    expect(openURL.mock.calls).toEqual([["https://github.com/gal064/muxflow/releases/latest"]]);
  });
});
