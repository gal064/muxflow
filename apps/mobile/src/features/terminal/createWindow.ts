// "New terminal" (design doc §9.4): TMUX_ACTION CREATE_WINDOW with the one
// stale_topology retry §7.5 asks for.

import { HostError, type HostConnection } from "../../protocol/HostConnection";
import { createWindow } from "../../protocol/requests";
import { log } from "../../session/log";
import type { SessionStore } from "../../store/sessionStore";

export interface CreatedWindow { windowId: string; paneId: string }

/**
 * How long a stale_topology retry waits for the newer TOPOLOGY_SNAPSHOT to
 * land. Returns the moment it does, so a fast link is unchanged; the ceiling
 * is link-sized (the desktop's `actionReconciliation.ts`, 1 s) because a 250 ms
 * wait was never met by a 300 ms round trip and an ordinary refusal reached
 * the user (docs/bugs/slow-link.md).
 */
export const STALE_TOPOLOGY_WAIT_MS = 1_000;

function waitForNewerGeneration(store: SessionStore, seen: bigint, timeoutMs: number): Promise<void> {
  return new Promise((resolve) => {
    if (store.getState().topologyGeneration > seen) return resolve();
    const timer = setTimeout(() => {
      unsubscribe();
      resolve();
    }, timeoutMs);
    const unsubscribe = store.subscribe((state) => {
      if (state.topologyGeneration > seen) {
        clearTimeout(timer);
        unsubscribe();
        resolve();
      }
    });
  });
}

/**
 * `expectedGeneration` is read from the store at call time. The host
 * reconciles topology in the background, so a `stale_topology` refusal is
 * ordinary; the fresh TOPOLOGY_SNAPSHOT travels on the ordered event channel
 * and can land just after the refusal, so the retry waits briefly for a newer
 * generation, then sends once more. A second refusal propagates.
 */
export async function createTerminalWindow(
  connection: HostConnection,
  store: SessionStore,
  sessionId: string,
  command = "",
  staleWaitMs = STALE_TOPOLOGY_WAIT_MS,
): Promise<CreatedWindow> {
  const kind = command.trim() ? "agent" : "terminal";
  let attemptNumber = 0;
  const attempt = async () => {
    attemptNumber += 1;
    const generation = store.getState().topologyGeneration;
    log(`create.window request kind=${kind} attempt=${attemptNumber} session=${sessionId} topology=${generation} commandPresent=${command.trim() ? "yes" : "no"}`);
    try {
      const response = await connection.request(createWindow(sessionId, connection.serverIdentity, generation, command));
      const result = response.tmuxActionResult;
      log(`create.window response kind=${kind} attempt=${attemptNumber} result=${result?.paneId ? "present" : "missing"} window=${result?.windowId || "none"} pane=${result?.paneId || "none"} responseTopology=${result?.topologyGeneration ?? 0n} storeTopology=${store.getState().topologyGeneration}`);
      return response;
    } catch (error) {
      log(`create.window refused kind=${kind} attempt=${attemptNumber} code=${diagnosticErrorCode(error)} storeTopology=${store.getState().topologyGeneration}`);
      throw Object.assign(error instanceof Error ? error : new Error(String(error)), { generation });
    }
  };
  let response;
  try {
    response = await attempt();
  } catch (error) {
    if (!(error instanceof HostError) || error.code !== "stale_topology") throw error;
    await waitForNewerGeneration(store, (error as { generation?: bigint }).generation ?? 0n, staleWaitMs);
    response = await attempt();
  }
  const result = response.tmuxActionResult;
  if (!result || !result.paneId) throw new HostError("missing_result", "the host did not return the new pane");
  return { windowId: result.windowId, paneId: result.paneId };
}

function diagnosticErrorCode(error: unknown): string {
  if (error instanceof HostError) return error.code;
  return error instanceof Error ? error.name : typeof error;
}
