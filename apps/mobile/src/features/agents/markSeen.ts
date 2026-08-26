// §9.3.1 / D7: tapping an agent that needs attention marks it seen on the host.

import { agentMarkSeen } from "../../protocol/requests";
import { getConnection } from "../../session/connectionManager";
import { log } from "../../session/log";
import { needsAttention } from "../../store/selectors";
import type { Agent } from "../../store/sessionStore";

export function markSeenIfNeeded(agent: Agent): void {
  if (!needsAttention(agent)) return;
  const connection = getConnection();
  if (!connection || connection.state !== "connected") return;
  connection.request(agentMarkSeen(agent.id, agent.attentionGeneration, connection.serverIdentity)).catch((error: unknown) => {
    log(`markSeen.failed ${agent.id} ${error instanceof Error ? error.message : String(error)}`);
  });
}
