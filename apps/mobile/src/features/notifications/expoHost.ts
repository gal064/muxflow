// The Android side of §13, over expo-notifications. The only file in this
// feature that imports a native module.
//
// Two notification channels exist in this app and they are not the same thing:
// `connection` is the foreground service's ongoing "Connected to …" (§6.3,
// created and owned by ConnectionService.kt — nothing here touches it) and
// `agents` is this one.

import * as Notifications from "expo-notifications";

import type { AgentNotification, NotificationHost, NotificationPermission } from "./host";
import { decodePayload, type TapTarget } from "./payload";
import { colors } from "../../ui/tokens";

/** §13: channel `agents`, importance HIGH, sound default, vibration default. */
export const AGENTS_CHANNEL_ID = "agents";

export function createExpoNotificationHost(): NotificationHost {
  return {
    async ensureChannel() {
      await Notifications.setNotificationChannelAsync(AGENTS_CHANNEL_ID, {
        name: "Agents",
        description: "An agent is blocked waiting for you, or has finished.",
        importance: Notifications.AndroidImportance.HIGH,
        // `sound` and `vibrationPattern` are deliberately absent: the channel
        // manager reads a missing sound as the system default notification
        // sound and a missing pattern as the system default vibration. A
        // `sound: "default"` here would be looked up as a bundled *file* named
        // "default" and resolve to silence.
        enableVibrate: true,
        lockscreenVisibility: Notifications.AndroidNotificationVisibility.PRIVATE,
        showBadge: true,
      });
    },

    async getPermission() {
      return classify(await Notifications.getPermissionsAsync());
    },

    async requestPermission() {
      return classify(await Notifications.requestPermissionsAsync());
    },

    async present(notification: AgentNotification) {
      await Notifications.scheduleNotificationAsync({
        // On Android expo-notifications posts with `tag = identifier`, so this
        // is §13's tag: re-posting replaces, `dismissNotificationAsync` cancels.
        identifier: notification.tag,
        content: {
          title: notification.title,
          body: notification.body,
          data: { ...notification.data },
          sound: "default",
          color: colors.accent,
          priority: Notifications.AndroidNotificationPriority.HIGH,
        },
        // A channel-aware trigger with no schedule: present now, on `agents`.
        trigger: { channelId: AGENTS_CHANNEL_ID },
      });
    },

    async cancel(tag: string) {
      await Notifications.dismissNotificationAsync(tag);
    },

    onTap(listener: (target: TapTarget) => void) {
      const deliver = (response: Notifications.NotificationResponse | null): void => {
        if (!response) return;
        // Not one of ours, or a payload we cannot route.
        const target = decodePayload(response.notification.request.content.data);
        if (target) listener(target);
      };
      // A tap that cold-started the app is waiting here, not on the listener.
      deliver(Notifications.getLastNotificationResponse());
      const subscription = Notifications.addNotificationResponseReceivedListener(deliver);
      return () => subscription.remove();
    },
  };
}

/**
 * §13's foreground rule is decided in `decide.ts`, not by the OS: anything that
 * reaches this handler survived step 6, so it is about a pane the user is not
 * looking at and is worth showing even with the app in front. Without a handler
 * expo-notifications shows nothing at all while the JS runtime is alive — and
 * the foreground service (§6.3) keeps it alive precisely when the app is in the
 * background, which is when these notifications matter.
 */
export function installForegroundPresentation(): void {
  Notifications.setNotificationHandler({
    handleNotification: async () => ({
      shouldShowBanner: true,
      shouldShowList: true,
      shouldPlaySound: true,
      shouldSetBadge: false,
    }),
  });
}

function classify(status: Notifications.NotificationPermissionsStatus): NotificationPermission {
  if (status.granted) return "granted";
  return status.canAskAgain ? "undetermined" : "denied";
}
