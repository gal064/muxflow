// The platform surface the agent notifier needs (design doc §13), behind an
// interface so the subscriber can be tested without a native module and so
// nothing here imports expo-notifications until the app actually starts.
//
// The Android implementation is `expoHost.ts`; tests supply their own.

import type { TapTarget } from "./payload";

/** §13 step 7's `data`, as it survives the JS ↔ native bridge (JSON: no bigint). */
export interface NotificationPayload {
  agentId: string;
  paneId: string;
  sessionId: string;
  /** Decimal string. */
  attentionGeneration: string;
}

export interface AgentNotification {
  /**
   * §13 step 7's tag. On Android this is the notification tag, so posting the
   * same tag again replaces the older notification for that agent (coalescing)
   * and `cancel(tag)` takes it down.
   */
  tag: string;
  title: string;
  body: string;
  data: NotificationPayload;
}

export type NotificationPermission = "granted" | "denied" | "undetermined";

export interface NotificationHost {
  /** Channel `agents`, importance HIGH, sound and vibration default (§13). */
  ensureChannel(): Promise<void>;
  getPermission(): Promise<NotificationPermission>;
  /** Raises the OS `POST_NOTIFICATIONS` prompt when it is still undetermined. */
  requestPermission(): Promise<NotificationPermission>;
  present(notification: AgentNotification): Promise<void>;
  cancel(tag: string): Promise<void>;
  /**
   * Taps, including the one that cold-started the app. Notifications whose
   * payload does not parse — the foreground service's ongoing one (§6.3) has
   * none — are dropped rather than delivered.
   */
  onTap(listener: (target: TapTarget) => void): () => void;
}
