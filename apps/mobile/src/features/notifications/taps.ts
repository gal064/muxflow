// §13: "Tap on a notification: open the app to `/terminal/{paneId}` and call
// `agentMarkSeen(agentId, attentionGeneration)`."
//
// Both halves have to survive a cold start, where the tap is what launched the
// process: the route is pushed once the navigator exists (`index.ts`), and the
// mark-seen waits for a connection rather than being dropped on the floor.

import type { TapTarget } from "./payload";
import { toRouteParam } from "../../navigation/routeParams";
import { agentMarkSeen } from "../../protocol/requests";
import { getConnection } from "../../session/connectionManager";
import { log } from "../../session/log";

export interface TerminalRoute {
  pathname: "/terminal/[paneId]";
  /** `sessionId` travels too: on a cold start the topology has not arrived yet
   * and the route would otherwise have no session to attach to (§9.5).
   * Both are `toRouteParam`-encoded, like every pushed param. */
  params: { paneId: string; sessionId: string };
}

export function terminalRoute(target: TapTarget): TerminalRoute {
  return { pathname: "/terminal/[paneId]", params: { paneId: toRouteParam(target.paneId), sessionId: toRouteParam(target.sessionId) } };
}

/** Sends one mark-seen. `false` means "no connection to send it on, hold it". */
export type MarkSeenSink = (target: TapTarget) => boolean;

export interface TapMarkSeen {
  request(target: TapTarget): void;
  /** Called when a connection reaches `connected`. */
  flush(): void;
  pending(): number;
}

/**
 * Deliberately not `agents/markSeen.ts::markSeenIfNeeded`: that one needs the
 * `Agent` record and gives up when there is no connection, and a tap has
 * neither — the store may not have the agent yet, and the generation to
 * acknowledge is the one the notification was posted for, not whatever the
 * store later holds.
 */
export function createTapMarkSeen(send: MarkSeenSink): TapMarkSeen {
  // Keyed by agent: only the newest generation per agent is worth sending.
  const held = new Map<string, TapTarget>();
  return {
    request(target) {
      if (!send(target)) held.set(target.agentId, target);
    },
    flush() {
      for (const [agentId, target] of [...held]) {
        if (send(target)) held.delete(agentId);
      }
    },
    pending() {
      return held.size;
    },
  };
}

export const connectionMarkSeenSink: MarkSeenSink = (target) => {
  const connection = getConnection();
  if (!connection || connection.state !== "connected") return false;
  // A held tap must not be flushed at whichever host happens to be connected
  // next: agent ids are hashed with the server identity, so it would be a
  // no-op there and stay unacknowledged here.
  if (target.serverIdentity && connection.serverIdentity !== target.serverIdentity) return false;
  connection
    .request(agentMarkSeen(target.agentId, target.attentionGeneration, connection.serverIdentity))
    .catch((error: unknown) => {
      log(`notifications markSeen.failed agent=${target.agentId} pane=${target.paneId} ${error instanceof Error ? error.message : String(error)}`);
    });
  return true;
};
