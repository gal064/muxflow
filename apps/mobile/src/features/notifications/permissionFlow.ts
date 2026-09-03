// §13's permission half: "Ask for POST_NOTIFICATIONS the first time a host
// reaches `connected`; if denied, show once on the Agents tab a dismissible
// banner." Separated from `index.ts` so the ordering is testable.

import type { NotificationHost, NotificationPermission } from "./host";

export interface PermissionFlowDeps {
  host: Pick<NotificationHost, "getPermission" | "requestPermission">;
  setPermission: (permission: NotificationPermission) => void;
}

export interface PermissionFlow {
  /** Every time a host reaches `connected`; only the first one asks. */
  onConnected(): Promise<void>;
  /** Every time the app comes back to the foreground. */
  onAppActive(): Promise<void>;
  asked(): boolean;
}

export function createPermissionFlow(deps: PermissionFlowDeps): PermissionFlow {
  let asked = false;
  return {
    async onConnected() {
      if (asked) return;
      asked = true;
      const current = await deps.host.getPermission();
      // Asking again once Android has stopped showing the prompt returns the
      // same answer without any UI, so the passive read decides.
      deps.setPermission(current === "undetermined" ? await deps.host.requestPermission() : current);
    },
    async onAppActive() {
      // The banner sends people to system settings, so the answer can change
      // behind the app's back — but only upwards. A passive read cannot
      // distinguish "refused" from "not asked yet" (Android reports both as
      // `canAskAgain`), so it may only ever clear the banner, never raise it.
      if (!asked) return;
      if ((await deps.host.getPermission()) === "granted") deps.setPermission("granted");
    },
    asked() {
      return asked;
    },
  };
}
