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
    case "idle":
      return isRecentIdle(agent, now) ? 3 : 4;
    default:
      return 5;
  }
}

/**
 * The one deadline at which an idle agent stops being Recent, or undefined
 * when the agent is not idle (an unread completion stays Recent until the
 * acknowledgement supplies a clock). Ported from the desktop's
 * `recentAgentExpiration` (`agentsList.ts`): counted from the acknowledgement
 * for a seen completion and from the lifecycle change otherwise.
 */
export function recentExpirationMs(agent: Agent): number | undefined {
  if (displayState(agent) !== "idle") return undefined;
  const recentSince = agent.attentionKind === "completed" && agent.attentionSeenAtMs > 0
    ? agent.attentionSeenAtMs
    : agent.lifecycleChangedAtMs;
  return recentSince + RECENT_WINDOW_MS;
}

/** Whether an idle agent is still inside its Recent window at `now`. */
export function isRecentIdle(agent: Agent, now: number): boolean {
  const expiration = recentExpirationMs(agent);
  return expiration !== undefined && now < expiration;
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

/** The two states that ask for a human. */
export type WaitingState = "blocked" | "done";

/**
 * The state that puts an agent in front of you, or undefined: blocked, or
 * finished without its pane having been looked at. The desktop's
 * `needsAttention(state)` (`agentsList.ts`) — what its bell counts, what its
 * unread "1" badge marks and what a workspace row's badge adds up. Note the
 * asymmetry with `needsAttention` above: looking at a blocked agent does not
 * answer it, so it stays waiting until its lifecycle moves on; looking at a
 * completion is all a completion asks for. A gone agent cannot wait on
 * anyone.
 */
export function waitingState(agent: Agent): WaitingState | undefined {
  if (!agent.present) return undefined;
  const state = displayState(agent);
  return state === "blocked" || state === "done" ? state : undefined;
}

/**
 * How many agents are waiting, and the loudest thing they are waiting for —
 * the desktop's workspace rollup (`selectors.ts` `deriveAgentRollups`, where
 * blocked outranks done) reduced to what one row paints. `loudest` is
 * undefined when nobody is waiting.
 */
export interface WaitingSummary {
  count: number;
  loudest: WaitingState | undefined;
}

export function summarizeWaiting(agents: Iterable<Agent>): WaitingSummary {
  let count = 0;
  let loudest: WaitingState | undefined;
  for (const agent of agents) {
    const state = waitingState(agent);
    if (!state) continue;
    count += 1;
    if (state === "blocked" || loudest === undefined) loudest = state;
  }
  return { count, loudest };
}

/** The number on the Agents tab: every waiting agent on the host (the desktop's `unreadCount`). */
export function waitingCount(state: Pick<SessionState, "agents">): number {
  return summarizeWaiting(Object.values(state.agents)).count;
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
