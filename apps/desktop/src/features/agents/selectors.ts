import type {
  AgentAttentionRollup,
  AgentDisplayState,
  AgentRecord,
  AgentRollups,
  AgentStoreState,
} from "./types";
import { generationIsAfter, type AgentGeneration } from "./generation";

const priority: Record<AgentAttentionRollup["state"], number> = {
  none: 0,
  idle: 1,
  unknown: 2,
  working: 3,
  done: 4,
  blocked: 5,
};

export function displayState(agent: AgentRecord): AgentDisplayState {
  return agent.lifecycle === "idle"
    && agent.attentionKind === "completed"
    && generationIsAfter(agent.attentionGeneration, agent.seenGeneration)
    ? "done"
    : agent.lifecycle;
}

export function agentsForScope(state: AgentStoreState, hostProfileId: string, serverIdentity?: string): AgentRecord[] {
  if (!state.authoritative || state.hostProfileId !== hostProfileId || state.serverIdentity !== serverIdentity) return [];
  return Object.values(state.byId).sort(compareAgents);
}

export function compareAgents(left: AgentRecord, right: AgentRecord): number {
  const stateDifference = priority[displayState(right)] - priority[displayState(left)];
  return stateDifference || right.updatedAt - left.updatedAt || left.displayName.localeCompare(right.displayName) || left.id.localeCompare(right.id);
}

export function deriveAgentRollups(agents: readonly AgentRecord[]): AgentRollups {
  const byAgent = new Map<string, AgentAttentionRollup>();
  const byPane = new Map<string, AgentAttentionRollup>();
  const byWindow = new Map<string, AgentAttentionRollup>();
  const byWorkspace = new Map<string, AgentAttentionRollup>();
  for (const agent of agents) {
    const own = rollup([agent]);
    byAgent.set(agent.id, own);
    if (agent.paneId) mergeInto(byPane, agent.paneId, own);
    if (agent.windowId) mergeInto(byWindow, agent.windowId, own);
    if (agent.sessionId) mergeInto(byWorkspace, agent.sessionId, own);
  }
  return { byAgent, byPane, byWindow, byWorkspace };
}

export function rollup(agents: readonly AgentRecord[]): AgentAttentionRollup {
  let result = emptyRollup();
  for (const agent of agents) result = combineRollups(result, singleRollup(displayState(agent)));
  return result;
}

function mergeInto(map: Map<string, AgentAttentionRollup>, key: string, next: AgentAttentionRollup): void {
  map.set(key, combineRollups(map.get(key) ?? emptyRollup(), next));
}

function emptyRollup(): AgentAttentionRollup {
  return { state: "none", blocked: 0, working: 0, done: 0, unknown: 0, idle: 0, total: 0 };
}

function singleRollup(state: AgentDisplayState): AgentAttentionRollup {
  return { ...emptyRollup(), state, [state]: 1, total: 1 };
}

function combineRollups(left: AgentAttentionRollup, right: AgentAttentionRollup): AgentAttentionRollup {
  return {
    state: priority[left.state] >= priority[right.state] ? left.state : right.state,
    blocked: left.blocked + right.blocked,
    working: left.working + right.working,
    done: left.done + right.done,
    unknown: left.unknown + right.unknown,
    idle: left.idle + right.idle,
    total: left.total + right.total,
  };
}

export function agentsMatchingFocusedPane(
  agents: readonly AgentRecord[],
  paneId: string | undefined,
): Array<{ agentId: string; attentionGeneration: AgentGeneration }> {
  if (!paneId) return [];
  return agents
    .filter((agent) => agent.paneId === paneId
      && generationIsAfter(agent.attentionGeneration, agent.seenGeneration))
    .map((agent) => ({ agentId: agent.id, attentionGeneration: agent.attentionGeneration }));
}
