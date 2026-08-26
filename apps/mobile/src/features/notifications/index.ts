// Starts §13. Called once, from `session/appWiring.ts::wireApp`.
//
// This is the only file in the feature that touches expo-notifications,
// expo-router or react-native; everything it wires together is testable
// without them.

import { router } from "expo-router";
import { AppState } from "react-native";

import { createExpoNotificationHost, installForegroundPresentation } from "./expoHost";
import { createAgentNotifier } from "./notifier";
import type { TapTarget } from "./payload";
import { createPermissionFlow } from "./permissionFlow";
import { notificationsUiStore } from "./permissionStore";
import { connectionMarkSeenSink, createTapMarkSeen, terminalRoute } from "./taps";
import { onAgentTransition } from "../../session/connectionManager";
import { log } from "../../session/log";
import { sessionStore } from "../../store/sessionStore";

let started = false;

export function startNotifications(): void {
  if (started) return;
  started = true;
  const host = createExpoNotificationHost();
  installForegroundPresentation();
  // §13: the channel exists from first launch, whether or not a host is added.
  void host.ensureChannel().catch(reportFailure("channel"));

  createAgentNotifier({
    host,
    getState: () => sessionStore.getState(),
    subscribe: (listener) => sessionStore.subscribe(listener),
    onAgentTransition,
    appInForeground: () => AppState.currentState === "active",
    log,
  }).start();

  const markSeen = createTapMarkSeen(connectionMarkSeenSink);
  host.onTap((target) => {
    log(`notifications: tap ${target.agentId} pane=${target.paneId}`);
    openTerminal(target);
    markSeen.request(target);
  });

  const permission = createPermissionFlow({
    host,
    setPermission: (outcome) => {
      log(`notifications: permission ${outcome}`);
      notificationsUiStore.getState().setPermission(outcome);
    },
  });
  sessionStore.subscribe((state) => {
    if (state.connection.state !== "connected") return;
    // A tap that cold-started the app had no connection to acknowledge on.
    markSeen.flush();
    void permission.onConnected().catch(reportFailure("permission"));
  });
  AppState.addEventListener("change", (next) => {
    if (next === "active") void permission.onAppActive().catch(reportFailure("permission"));
  });
}

/**
 * A tap can arrive before the navigator exists — it is what launched the
 * process. expo-router throws until the root layout has mounted, so the push is
 * retried on a short leash rather than lost.
 */
function openTerminal(target: TapTarget, attempt = 0): void {
  try {
    router.navigate(terminalRoute(target));
  } catch (error: unknown) {
    if (attempt >= 30) {
      log(`notifications: tap.navigate.failed ${error instanceof Error ? error.message : String(error)}`);
      return;
    }
    setTimeout(() => openTerminal(target, attempt + 1), 100);
  }
}

function reportFailure(what: string): (error: unknown) => void {
  return (error) => log(`notifications: ${what}.failed ${error instanceof Error ? error.message : String(error)}`);
}
