import { beforeEach, describe, expect, it, vi } from "vitest";

// The one test that stands between `expoHost.ts` and the device: everything
// expo-notifications is asked to do, checked against §13.
const notifications = vi.hoisted(() => ({
  AndroidImportance: { UNKNOWN: 0, UNSPECIFIED: 1, NONE: 2, MIN: 3, LOW: 4, DEFAULT: 5, HIGH: 6, MAX: 7 },
  AndroidNotificationPriority: { MIN: "min", LOW: "low", DEFAULT: "default", HIGH: "high", MAX: "max" },
  AndroidNotificationVisibility: { UNKNOWN: 0, PUBLIC: 1, PRIVATE: 2, SECRET: 3 },
  setNotificationChannelAsync: vi.fn(async () => null),
  setNotificationHandler: vi.fn(),
  getPermissionsAsync: vi.fn(async () => ({ granted: true, canAskAgain: true })),
  requestPermissionsAsync: vi.fn(async () => ({ granted: true, canAskAgain: true })),
  scheduleNotificationAsync: vi.fn(async () => "id"),
  dismissNotificationAsync: vi.fn(async () => {}),
  getLastNotificationResponse: vi.fn(() => null as unknown),
  clearLastNotificationResponse: vi.fn(),
  addNotificationResponseReceivedListener: vi.fn((_listener: (response: unknown) => void) => ({ remove: vi.fn() })),
}));
vi.mock("expo-notifications", () => notifications);

const { AGENTS_CHANNEL_ID, createExpoNotificationHost, installForegroundPresentation } = await import("./expoHost");
const { colors } = await import("../../ui/tokens");

const response = (data: unknown) => ({ notification: { request: { content: { data } } } });
const payload = { agentId: "a1", paneId: "%12", sessionId: "$3", attentionGeneration: "7", serverIdentity: "tmux:/s:1" };

describe("the Android notification host (§13)", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    notifications.getLastNotificationResponse.mockReturnValue(null);
  });

  it("creates channel `agents` at importance HIGH with the system sound and vibration", async () => {
    await createExpoNotificationHost().ensureChannel();
    expect(notifications.setNotificationChannelAsync).toHaveBeenCalledTimes(1);
    const [id, channel] = notifications.setNotificationChannelAsync.mock.calls[0] as unknown as [string, Record<string, unknown>];
    expect(id).toBe("agents");
    expect(channel.importance).toBe(notifications.AndroidImportance.HIGH);
    expect(channel.enableVibrate).toBe(true);
    // Absent, not null: the channel manager reads a missing key as "the system
    // default" and an explicit value as a bundled file name.
    expect("sound" in channel).toBe(false);
    expect("vibrationPattern" in channel).toBe(false);
  });

  it("presents on the agents channel with the agent id as the tag", async () => {
    await createExpoNotificationHost().present({
      tag: "a1",
      title: "dev-app · Claude",
      body: "Needs your input",
      data: payload,
    });
    const [request] = notifications.scheduleNotificationAsync.mock.calls[0] as unknown as [Record<string, unknown>];
    expect(request).toMatchObject({
      identifier: "a1",
      content: {
        title: "dev-app · Claude",
        body: "Needs your input",
        data: payload,
        color: colors.accent,
        priority: notifications.AndroidNotificationPriority.HIGH,
      },
      trigger: { channelId: AGENTS_CHANNEL_ID },
    });
  });

  it("cancels by tag", async () => {
    await createExpoNotificationHost().cancel("a1");
    expect(notifications.dismissNotificationAsync).toHaveBeenCalledWith("a1");
  });

  it("reads the current permission, leaving `undetermined` for what has not been asked", async () => {
    const host = createExpoNotificationHost();
    notifications.getPermissionsAsync.mockResolvedValueOnce({ granted: true, canAskAgain: false });
    expect(await host.getPermission()).toBe("granted");
    notifications.getPermissionsAsync.mockResolvedValueOnce({ granted: false, canAskAgain: true });
    expect(await host.getPermission()).toBe("undetermined");
    notifications.getPermissionsAsync.mockResolvedValueOnce({ granted: false, canAskAgain: false });
    expect(await host.getPermission()).toBe("denied");
  });

  it("treats a refused prompt as denied even while Android would still ask again", async () => {
    const host = createExpoNotificationHost();
    // Android keeps `canAskAgain` true after the first "Don't allow"; §13's
    // banner has to appear then, not only after the second refusal.
    notifications.requestPermissionsAsync.mockResolvedValueOnce({ granted: false, canAskAgain: true });
    expect(await host.requestPermission()).toBe("denied");
    notifications.requestPermissionsAsync.mockResolvedValueOnce({ granted: true, canAskAgain: false });
    expect(await host.requestPermission()).toBe("granted");
  });

  describe("taps", () => {
    it("delivers the tap that cold-started the app, decoded, and clears it", () => {
      notifications.getLastNotificationResponse.mockReturnValue(response(payload));
      const seen: unknown[] = [];
      createExpoNotificationHost().onTap((target) => seen.push(target));
      expect(seen).toEqual([{ agentId: "a1", paneId: "%12", sessionId: "$3", attentionGeneration: 7n, serverIdentity: "tmux:/s:1" }]);
      // Otherwise Android replays it from the Activity's launch intent every
      // time the task is restored from Recents.
      expect(notifications.clearLastNotificationResponse).toHaveBeenCalledTimes(1);
    });

    it("leaves the stored response alone when the app was not launched by a tap", () => {
      createExpoNotificationHost().onTap(() => {});
      expect(notifications.clearLastNotificationResponse).not.toHaveBeenCalled();
    });

    it("delivers later taps and drops notifications that are not ours", () => {
      const seen: unknown[] = [];
      createExpoNotificationHost().onTap((target) => seen.push(target));
      const listener = notifications.addNotificationResponseReceivedListener.mock.calls[0]?.[0] as (r: unknown) => void;
      listener(response({}));
      listener(null);
      expect(seen).toEqual([]);
      listener(response(payload));
      expect(seen).toHaveLength(1);
    });

    it("removes its subscription", () => {
      const remove = vi.fn();
      notifications.addNotificationResponseReceivedListener.mockReturnValueOnce({ remove });
      createExpoNotificationHost().onTap(() => {})();
      expect(remove).toHaveBeenCalledTimes(1);
    });
  });

  it("shows what survived step 6, even with the app in front", async () => {
    installForegroundPresentation();
    const handler = notifications.setNotificationHandler.mock.calls[0]?.[0] as {
      handleNotification: () => Promise<Record<string, boolean>>;
    };
    expect(await handler.handleNotification()).toMatchObject({ shouldShowBanner: true, shouldShowList: true, shouldPlaySound: true });
  });
});
