// Agent display logic (design doc §8.2). `displayState` is ported from
// apps/desktop/src/features/agents/selectors.ts; the sort order is the doc's.

import { stripAgentStatusGlyphs } from "../features/agents/agentLabels";
import type { Agent, SessionState } from "./sessionStore";

export type AgentDisplayState = Agent["lifecycle"] | "done";

export function needsAttention(agent: Agent): boolean {
  return agent.attentionGeneration > agent.seenGeneration;
}

export function displayState(agent: Agent): AgentDisplayState {
  return agent.lifecycle === "idle" && agent.attentionKind === "completed" && needsAttention(agent)
    ? "done"
    : agent.lifecycle;
}

/** Lower sorts first: blocked+needsAttention, blocked, done, working, idle, unknown. */
function rank(agent: Agent): number {
  const state = displayState(agent);
  if (state === "blocked") return needsAttention(agent) ? 0 : 1;
  switch (state) {
    case "done":
      return 2;
    case "working":
      return 3;
    case "idle":
      return 4;
    default:
      return 5;
  }
}

export function compareAgents(left: Agent, right: Agent): number {
  // Agents that are gone are shown last, whatever their retained state.
  if (left.present !== right.present) return left.present ? -1 : 1;
  return rank(left) - rank(right)
    || right.updatedAtMs - left.updatedAtMs
    || left.displayName.localeCompare(right.displayName)
    || left.id.localeCompare(right.id);
}

export function sortedAgents(state: Pick<SessionState, "agents">): Agent[] {
  return Object.values(state.agents).sort(compareAgents);
}

export function agentWorkspaceName(state: Pick<SessionState, "sessions">, agent: Agent): string {
  return stripAgentStatusGlyphs(state.sessions[agent.route.sessionId]?.name ?? agent.route.sessionNameFallback);
}

export function agentWindowName(state: Pick<SessionState, "windows">, agent: Agent): string {
  return stripAgentStatusGlyphs(state.windows[agent.route.windowId]?.name ?? agent.route.windowNameFallback);
}

export function blockedAgentCount(state: Pick<SessionState, "agents">): number {
  return Object.values(state.agents).filter((agent) => agent.present && displayState(agent) === "blocked").length;
}

/**
 * Whether an agent sits in the leading "Pinned" block: its workspace or its
 * tab is pinned on the host (same rule as the desktop agents panel,
 * `apps/desktop/src/features/agents/agentsList.ts`).
 */
export function agentPinned(state: Pick<SessionState, "sessions" | "windows">, agent: Agent): boolean {
  return Boolean(state.sessions[agent.route.sessionId]?.pinned || state.windows[agent.route.windowId]?.pinned);
}

/** Priority order, with the pinned rows lifted to the front in that same order. */
export function sortedAgentsPinnedFirst(state: Pick<SessionState, "agents" | "sessions" | "windows">): Agent[] {
  const agents = sortedAgents(state);
  return [...agents.filter((agent) => agentPinned(state, agent)), ...agents.filter((agent) => !agentPinned(state, agent))];
}

/**
 * Where the "Pinned" and (when both exist) trailing dividers go in a list
 * already ordered pinned-first: the index of the first row in each block.
 * Copies the desktop sidebar's two-divider presentation.
 */
export function pinnedDividers(pinnedFlags: readonly boolean[]): { pinnedAt: number | null; restAt: number | null } {
  const pinnedCount = pinnedFlags.filter(Boolean).length;
  if (pinnedCount === 0) return { pinnedAt: null, restAt: null };
  return { pinnedAt: 0, restAt: pinnedCount < pinnedFlags.length ? pinnedCount : null };
}
