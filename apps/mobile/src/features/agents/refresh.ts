// Pull-to-refresh (design.md §9): AGENT_SNAPSHOT, re-applied to the store.

import { agentSnapshot } from "../../protocol/requests";
import { getConnection, toast } from "../../session/connectionManager";
import { sessionStore } from "../../store/sessionStore";

export async function refreshAgents(): Promise<void> {
  const connection = getConnection();
  if (!connection || connection.state !== "connected") return;
  try {
    const response = await connection.request(agentSnapshot(connection.serverIdentity));
    const snapshot = response.agent?.snapshot;
    if (snapshot) sessionStore.getState().applyAgentSnapshot(snapshot);
  } catch (error) {
    toast(error instanceof Error ? error.message : String(error));
  }
}
