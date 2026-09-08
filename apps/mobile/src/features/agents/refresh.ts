// Pull-to-refresh (design.md §9): AGENT_SNAPSHOT, re-applied to the store.

import { agentSnapshot } from "../../protocol/requests";
import { emitAgentTransition, getConnection, toast } from "../../session/connectionManager";
import { log } from "../../session/log";
import { sessionStore } from "../../store/sessionStore";
import { logAgentTransitions } from "./diagnostics";

export async function refreshAgents(): Promise<void> {
  const connection = getConnection();
  if (!connection || connection.state !== "connected") return;
  try {
    const response = await connection.request(agentSnapshot(connection.serverIdentity));
    const snapshot = response.agent?.snapshot;
    if (!snapshot) return;
    // A reconciling snapshot feeds §13's decision rule like any AGENT_STATE.
    const previousAgents = sessionStore.getState().agents;
    const transitions = sessionStore.getState().applyAgentSnapshot(snapshot);
    logAgentTransitions(previousAgents, sessionStore.getState().agents, "refresh", sessionStore.getState().topologyGeneration, log);
    for (const transition of transitions) {
      emitAgentTransition(transition);
    }
  } catch (error) {
    toast(error instanceof Error ? error.message : String(error));
  }
}
