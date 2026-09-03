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
import { backgroundSleep } from "../../session/backgroundTimer";
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
    // The post-settle wait runs in the background too — a cancel for an agent
    // seen on the desktop must take the notification down without the app
    // being opened — and a JS timer would not (see `backgroundTimer.ts`).
    sleep: backgroundSleep,
    log,
  }).start();

  const markSeen = createTapMarkSeen(connectionMarkSeenSink);
  host.onTap((target) => {
    const identity = sessionStore.getState().serverIdentity;
    // `%12` exists on every tmux server. If this app is already looking at a
    // different one, routing the tap would open an unrelated pane; hold the
    // acknowledgement instead and let it flush if that host comes back.
    if (identity && target.serverIdentity && identity !== target.serverIdentity) {
      log(`notifications: tap ${target.agentId} ignored (other host)`);
      markSeen.request(target);
      return;
    }
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
  let wasConnected = false;
  sessionStore.subscribe((state) => {
    const connected = state.connection.state === "connected";
    if (connected === wasConnected) return;
    wasConnected = connected;
    if (!connected) return;
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
 * process. `router.navigate` puts the link on expo-router's `routingQueue`,
 * which `useImperativeApiEmitter` drains once the root layout has mounted, so
 * the cold-start push needs no retry of its own.
 */
function openTerminal(target: TapTarget): void {
  try {
    router.navigate(terminalRoute(target));
  } catch (error: unknown) {
    log(`notifications: tap.navigate.failed ${error instanceof Error ? error.message : String(error)}`);
  }
}

function reportFailure(what: string): (error: unknown) => void {
  return (error) => log(`notifications: ${what}.failed ${error instanceof Error ? error.message : String(error)}`);
}
