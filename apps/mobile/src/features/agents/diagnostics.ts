import type { Agent } from "../../store/sessionStore";

export type AgentMap = Readonly<Record<string, Agent>>;

/** Logs only presence/lifecycle edges, not route-only or repeated snapshots. */
export function logAgentTransitions(
  previous: AgentMap,
  next: AgentMap,
  source: string,
  topologyGeneration: bigint,
  log: (line: string) => void,
): void {
  const ids = new Set([...Object.keys(previous), ...Object.keys(next)]);
  for (const agentId of ids) {
    const before = previous[agentId];
    const after = next[agentId];
    const beforePresent = before?.present ?? false;
    const afterPresent = after?.present ?? false;
    const beforeLifecycle = before?.lifecycle ?? "absent";
    const afterLifecycle = after?.lifecycle ?? "absent";
    if (before && after && beforePresent === afterPresent && beforeLifecycle === afterLifecycle) continue;
    const paneId = after?.route.paneId ?? before?.route.paneId ?? "unknown";
    const stateGeneration = after?.stateGeneration ?? before?.stateGeneration ?? 0n;
    log(`agent.transition source=${source.replace(/\s+/g, "-")} agent=${agentId} pane=${paneId} present=${beforePresent}->${afterPresent} lifecycle=${beforeLifecycle}->${afterLifecycle} stateGeneration=${stateGeneration} topology=${topologyGeneration}`);
  }
}
