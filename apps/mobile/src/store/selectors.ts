// Agent display logic (design doc §8.2). `displayState` is ported from
// apps/desktop/src/features/agents/selectors.ts; the sort order is the doc's.

import { stripAgentStatusGlyphs } from "../features/agents/agentLabels";
import type { Agent, SessionState } from "./sessionStore";

export type AgentDisplayState = Agent["lifecycle"] | "done";

/** How long an acknowledged completed or freshly idle agent stays near the active work. */
export const RECENT_WINDOW_MS = 4 * 60 * 60 * 1_000;

export function needsAttention(agent: Agent): boolean {
  return agent.attentionGeneration > agent.seenGeneration;
}

export function displayState(agent: Agent): AgentDisplayState {
  return agent.lifecycle === "idle" && agent.attentionKind === "completed" && needsAttention(agent)
    ? "done"
    : agent.lifecycle;
}

/**
 * Lower sorts first: blocked+needsAttention, blocked, working, recent, idle,
 * unknown — the desktop's priority buckets (`agentsList.ts`). "Done" shares
 * the recent bucket: acknowledging a completion changes only the row's dot and
 * badge, so the tap that clears them does not move the row out from under the
 * finger. A freshly idle agent stays recent for `RECENT_WINDOW_MS`, counted
 * from the acknowledgement for a seen completion and from the lifecycle change
 * otherwise.
 */
function rank(agent: Agent, now: number): number {
  const state = displayState(agent);
  if (state === "blocked") return needsAttention(agent) ? 0 : 1;
  switch (state) {
    case "working":
      return 2;
    case "done":
      return 3;
    case "idle": {
      const recentSince = agent.attentionKind === "completed" && agent.attentionSeenAtMs > 0
        ? agent.attentionSeenAtMs
        : agent.lifecycleChangedAtMs;
      return now - recentSince < RECENT_WINDOW_MS ? 3 : 4;
    }
    default:
      return 5;
  }
}

export function compareAgents(left: Agent, right: Agent, now = Date.now()): number {
  // Agents that are gone are shown last, whatever their retained state.
  if (left.present !== right.present) return left.present ? -1 : 1;
  // Inside a state, only a real lifecycle transition changes recency, so
  // repeated hooks and route-only updates cannot reshuffle rows.
  return rank(left, now) - rank(right, now)
    || right.lifecycleChangedAtMs - left.lifecycleChangedAtMs
    || left.displayName.localeCompare(right.displayName)
    || left.id.localeCompare(right.id);
}

/**
 * The Agents tab order: status first, and inside each status the pinned rows
 * lead their unpinned peers — a pin organises peers within a status, it does
 * not outrank a louder status (the desktop's `byStatus` in
 * `apps/desktop/src/features/agents/agentsList.ts`).
 */
export function sortedAgents(state: Pick<SessionState, "agents" | "sessions" | "windows">, now = Date.now()): Agent[] {
  return Object.values(state.agents).sort((left, right) => {
    if (left.present !== right.present) return left.present ? -1 : 1;
    return rank(left, now) - rank(right, now)
      || Number(agentPinned(state, right)) - Number(agentPinned(state, left))
      || compareAgents(left, right, now);
  });
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
 * Whether an agent's workspace or tab is pinned on the host. Used to lead its
 * status peers in `sortedAgents`.
 */
export function agentPinned(state: Pick<SessionState, "sessions" | "windows">, agent: Agent): boolean {
  return Boolean(state.sessions[agent.route.sessionId]?.pinned || state.windows[agent.route.windowId]?.pinned);
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
