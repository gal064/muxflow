import { beforeEach, describe, expect, it, vi } from "vitest";

const { platform, openURL, download, startActivity, cached, toast } = vi.hoisted(() => ({
  platform: { OS: "ios" },
  openURL: vi.fn().mockResolvedValue(undefined),
  download: vi.fn(),
  startActivity: vi.fn(),
  cached: { exists: false, delete: vi.fn() },
  toast: vi.fn(),
}));
vi.mock("react-native", () => ({
  Platform: platform,
  Linking: { openURL },
  AppState: { currentState: "active", addEventListener: vi.fn() },
}));
vi.mock("expo-constants", () => ({ default: { expoConfig: { version: "0.1.9" } } }));
vi.mock("expo-file-system", () => ({
  File: Object.assign(function File() { return cached; }, { downloadFileAsync: download }),
  Paths: { cache: "cache" },
}));
vi.mock("expo-intent-launcher", () => ({ startActivityAsync: startActivity }));
vi.mock("../../session/log", () => ({ log: vi.fn() }));
vi.mock("../../session/connectionManager", () => ({ toast }));

import { installAppUpdate, openAppUpdates, openDesktopUpdates, updateStore } from "./index";

const RELEASE_PAGE = "https://github.com/gal064/muxflow/releases/latest";

describe("distribution update destinations", () => {
  beforeEach(() => {
    platform.OS = "ios";
    openURL.mockReset().mockResolvedValue(undefined);
    download.mockReset().mockResolvedValue({ contentUri: "content://apk" });
    startActivity.mockReset().mockResolvedValue({ resultCode: 0 });
    updateStore.setState({ update: null, downloading: false });
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

  it("opens the release page on Android when no update is offered", async () => {
    platform.OS = "android";
    await openAppUpdates();
    expect(openURL.mock.calls).toEqual([[RELEASE_PAGE]]);
    expect(download).not.toHaveBeenCalled();
  });

  it("downloads the offered APK and hands it to Android's installer", async () => {
    platform.OS = "android";
    updateStore.setState({ update: { version: "0.2.0" } });
    const installing = openAppUpdates();
    expect(updateStore.getState().downloading).toBe(true);
    expect(toast).toHaveBeenCalledWith("Keep Muxflow open while the update downloads.");
    await installing;
    expect(download.mock.calls[0]?.[0]).toBe("https://github.com/gal064/muxflow/releases/latest/download/muxflow-0.2.0-android.apk");
    expect(startActivity).toHaveBeenCalledWith("android.intent.action.VIEW", {
      data: "content://apk",
      flags: 1,
      type: "application/vnd.android.package-archive",
    });
    expect(openURL).not.toHaveBeenCalled();
    expect(updateStore.getState().downloading).toBe(false);
  });

  it("ignores a second tap while downloading", async () => {
    updateStore.setState({ update: { version: "0.2.0" } });
    const first = installAppUpdate();
    await installAppUpdate();
    await first;
    expect(download).toHaveBeenCalledTimes(1);
  });

  it.each([
    ["download", () => download.mockRejectedValueOnce(new Error("HTTP 404"))],
    ["installer", () => startActivity.mockRejectedValueOnce(new Error("no activity"))],
  ])("falls back to the release page when the %s fails", async (_step, fail) => {
    fail();
    updateStore.setState({ update: { version: "0.2.0" } });
    await installAppUpdate();
    expect(openURL.mock.calls).toEqual([[RELEASE_PAGE]]);
    expect(updateStore.getState().downloading).toBe(false);
  });
});
