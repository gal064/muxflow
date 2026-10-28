// "Close agent", "Close terminal" and "Close workspace" from the hold sheet
// (design doc §9.3, §9.4): TMUX_ACTION CLOSE_PANE / CLOSE_WINDOW /
// CLOSE_SESSION, sent without a dialog. "Pin …" and "Unpin …" there send
// SET_PINNED the same way.

import { HostError, type HostConnection } from "../../protocol/HostConnection";
import { closeTmuxTarget, setPinnedTarget, type CloseTarget, type PinTarget } from "../../protocol/requests";
import type { Request } from "../../protocol/gen/envelope_pb";
import { log } from "../../session/log";

export function closeTarget(connection: HostConnection, target: CloseTarget, expectedServerIdentity: string): Promise<void> {
  return sendHeldAction(connection, "close.target", target, () => closeTmuxTarget(target, expectedServerIdentity));
}

export function pinTarget(connection: HostConnection, target: PinTarget, pinned: boolean, expectedServerIdentity: string): Promise<void> {
  return sendHeldAction(connection, pinned ? "pin.target" : "unpin.target", target, () => setPinnedTarget(target, pinned, expectedServerIdentity));
}

/**
 * The host refuses any action with `stale_topology` when its fresh discovery
 * differs structurally from the baseline it last published (a resize is
 * enough), and publishes the new baseline before answering. Nothing ran, and
 * these actions pin no generation, so it is sent once more straight away. A
 * second refusal propagates. The server identity is the one captured with the
 * target, never re-read: a newer server can reuse its ids.
 */
async function sendHeldAction(connection: HostConnection, name: string, target: CloseTarget, request: () => Request): Promise<void> {
  const id = targetId(target);
  for (let attempt = 1; ; attempt += 1) {
    log(`${name} request kind=${target.kind} attempt=${attempt} target=${id}`);
    try {
      await connection.request(request());
      log(`${name} response kind=${target.kind} attempt=${attempt} target=${id}`);
      return;
    } catch (error) {
      log(`${name} refused kind=${target.kind} attempt=${attempt} target=${id} code=${error instanceof HostError ? error.code : error instanceof Error ? error.name : typeof error}`);
      if (attempt > 1 || !(error instanceof HostError) || error.code !== "stale_topology") throw error;
    }
  }
}

function targetId(target: CloseTarget): string {
  switch (target.kind) {
    case "session": return target.sessionId;
    case "window": return target.windowId;
    case "pane": return target.paneId;
  }
}
