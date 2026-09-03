// Pull-to-refresh (design.md §9): AGENT_SNAPSHOT, re-applied to the store.

import { agentSnapshot } from "../../protocol/requests";
import { emitAgentTransition, getConnection, toast } from "../../session/connectionManager";
import { sessionStore } from "../../store/sessionStore";

export async function refreshAgents(): Promise<void> {
  const connection = getConnection();
  if (!connection || connection.state !== "connected") return;
  try {
    const response = await connection.request(agentSnapshot(connection.serverIdentity));
    const snapshot = response.agent?.snapshot;
    if (!snapshot) return;
    // A reconciling snapshot feeds §13's decision rule like any AGENT_STATE.
    for (const transition of sessionStore.getState().applyAgentSnapshot(snapshot)) {
      emitAgentTransition(transition);
    }
  } catch (error) {
    toast(error instanceof Error ? error.message : String(error));
  }
}
