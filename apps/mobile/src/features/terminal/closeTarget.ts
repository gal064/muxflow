// "Close agent", "Close terminal" and "Close workspace" from the hold sheet
// (design doc §9.3, §9.4): TMUX_ACTION CLOSE_PANE / CLOSE_WINDOW /
// CLOSE_SESSION, sent without a dialog.

import { HostError, type HostConnection } from "../../protocol/HostConnection";
import { closeTmuxTarget, type CloseTarget } from "../../protocol/requests";
import { log } from "../../session/log";

/**
 * The host refuses any action with `stale_topology` when its fresh discovery
 * differs structurally from the baseline it last published (a resize is
 * enough), and publishes the new baseline before answering. Nothing ran, and
 * the close pins no generation, so it is sent once more straight away. A
 * second refusal propagates. `expectedServerIdentity` is the one captured
 * with the target, never re-read: a newer server can reuse its ids.
 */
export async function closeTarget(connection: HostConnection, target: CloseTarget, expectedServerIdentity: string): Promise<void> {
  const id = targetId(target);
  for (let attempt = 1; ; attempt += 1) {
    log(`close.target request kind=${target.kind} attempt=${attempt} target=${id}`);
    try {
      await connection.request(closeTmuxTarget(target, expectedServerIdentity));
      log(`close.target response kind=${target.kind} attempt=${attempt} target=${id}`);
      return;
    } catch (error) {
      log(`close.target refused kind=${target.kind} attempt=${attempt} target=${id} code=${error instanceof HostError ? error.code : error instanceof Error ? error.name : typeof error}`);
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
