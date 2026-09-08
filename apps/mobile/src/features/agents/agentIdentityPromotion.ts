import type { AgentEvent } from "../../protocol/gen/envelope_pb";
import type { Agent } from "../../store/sessionStore";

export interface AgentIdentityPromotion {
  retiredAgentIds: string[];
  agent: Agent;
}

/**
 * Recognize only an atomic authoritative identity handoff: the event itself
 * retires an existing same-adapter agent on the same non-empty pane while
 * inserting the replacement. Independent events and pane reuse never qualify.
 */
export function deriveAgentIdentityPromotion(
  previousAgents: Readonly<Record<string, Agent>>,
  event: AgentEvent,
  next: Agent,
): AgentIdentityPromotion | undefined {
  if (!event.agent || event.agent.agentId !== next.id || !next.route.paneId) return undefined;
  const retiredAgentIds = [...new Set(event.retiredAgentIds)].filter((retiredId) => {
    if (retiredId === next.id) return false;
    const retired = previousAgents[retiredId];
    return retired !== undefined
      && retired.adapterId === next.adapterId
      && retired.route.paneId !== ""
      && retired.route.paneId === next.route.paneId;
  });
  return retiredAgentIds.length > 0 ? { retiredAgentIds, agent: next } : undefined;
}
