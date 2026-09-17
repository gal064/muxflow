// What the Agents tab needs to know about §13's permission banner.
//
// §13: "Ask for POST_NOTIFICATIONS the first time a host reaches connected; if
// denied, show once on the Agents tab a dismissible banner." The asking lives
// in `index.ts`; this is the one bit of it a screen reads.

import { createStore, type StoreApi } from "zustand/vanilla";

import type { NotificationPermission } from "./host";

export interface NotificationsUiState {
  /** `unknown` until the app has asked the OS. */
  permission: NotificationPermission | "unknown";
  bannerDismissed: boolean;
  setPermission(permission: NotificationPermission): void;
  dismissBanner(): void;
}

export type NotificationsUiStore = StoreApi<NotificationsUiState>;

export function createNotificationsUiStore(): NotificationsUiStore {
  return createStore<NotificationsUiState>((set) => ({
    permission: "unknown",
    bannerDismissed: false,
    setPermission(permission) {
      set({ permission });
    },
    dismissBanner() {
      set({ bannerDismissed: true });
    },
  }));
}

export const notificationsUiStore: NotificationsUiStore = createNotificationsUiStore();

/** "Show once": until the user dismisses it, and never while notifications work. */
export function showNotificationsOffBanner(state: NotificationsUiState): boolean {
  return state.permission === "denied" && !state.bannerDismissed;
}
