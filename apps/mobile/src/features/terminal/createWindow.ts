// "New terminal" (design doc §9.4): TMUX_ACTION CREATE_WINDOW with the one
// stale_topology retry §7.5 asks for.

import { HostError, type HostConnection } from "../../protocol/HostConnection";
import { createWindow } from "../../protocol/requests";
import type { SessionStore } from "../../store/sessionStore";

export interface CreatedWindow { windowId: string; paneId: string }

/**
 * `expectedGeneration` is read from the store at call time. The host
 * reconciles topology in the background, so a `stale_topology` refusal is
 * ordinary; it arrives after a fresh TOPOLOGY_SNAPSHOT, so one retry with the
 * new generation is enough. A second refusal propagates.
 */
export async function createTerminalWindow(connection: HostConnection, store: SessionStore, sessionId: string): Promise<CreatedWindow> {
  const attempt = () => connection.request(createWindow(sessionId, connection.serverIdentity, store.getState().topologyGeneration));
  let response;
  try {
    response = await attempt();
  } catch (error) {
    if (!(error instanceof HostError) || error.code !== "stale_topology") throw error;
    response = await attempt();
  }
  const result = response.tmuxActionResult;
  if (!result || !result.paneId) throw new HostError("missing_result", "the host did not return the new pane");
  return { windowId: result.windowId, paneId: result.paneId };
}
