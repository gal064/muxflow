// Row copy for the Agents / Workspaces / Workspace screens (design.md §9.3, §9.4).

import { displayState, summarizeWaiting, type WaitingState, type WaitingSummary } from "../../store/selectors";
import type { Agent, SessionState } from "../../store/sessionStore";
import type { PillState } from "../../ui/components/StatusPill";
import { colors } from "../../ui/tokens";
import { agentSessionLabel, withoutStatusGlyphs } from "./agentLabels";

export function agentPillState(agent: Agent): PillState {
  return agent.present ? displayState(agent) : "gone";
}

/**
 * The row's name (§9.3.1): the tmux tab, the desktop's `agentSessionLabel` —
 * status glyphs stripped, a generic or UUID-like name passed over for the
 * assigned name, then the adapter's. The adapter itself is the icon's job.
 * An agent with neither a window nor a name (gone, its window closed, or a
 * title that is only a ticker frame) says "{adapter} in pane {index}" so the
 * row still points somewhere.
 */
export function agentTitle(state: Pick<SessionState, "windows" | "adapters">, agent: Agent): string {
  const windowName = state.windows[agent.route.windowId]?.name ?? agent.route.windowNameFallback;
  if (!withoutStatusGlyphs(windowName) && !agent.displayName.trim()) return `${adapterDisplayName(state, agent)} in pane ${agent.route.paneIndexFallback}`;
  return agentSessionLabel({ windowName, displayName: agent.displayName, adapterId: agent.adapterId }, state.adapters);
}

/**
 * Who an agent is, apart from where it sits (§9.4's line 2, where the row's
 * title is already the window): the assigned name — the adapter's unless the
 * user renamed it — or the adapter's own.
 */
export function agentDisplayName(state: Pick<SessionState, "adapters">, agent: Agent): string {
  return agent.displayName.trim() || adapterDisplayName(state, agent);
}

function adapterDisplayName(state: Pick<SessionState, "adapters">, agent: Agent): string {
  return state.adapters.find((a) => a.id === agent.adapterId)?.displayName ?? agent.adapterId;
}

export function agentsInWindow(state: Pick<SessionState, "agents">, windowId: string): Agent[] {
  return Object.values(state.agents).filter((agent) => agent.present && agent.route.windowId === windowId);
}

export function agentForPane(state: Pick<SessionState, "agents">, paneId: string): Agent | undefined {
  return Object.values(state.agents).find((agent) => agent.present && agent.route.paneId === paneId);
}

/** The loudest pill state for a window: blocked > done > working > idle > unknown (§9.4). */
const LOUDNESS: PillState[] = ["blocked", "done", "working", "idle", "unknown", "gone"];
export function loudestPill(agents: Agent[]): PillState | undefined {
  let best: PillState | undefined;
  for (const agent of agents) {
    const state = agentPillState(agent);
    if (best === undefined || LOUDNESS.indexOf(state) < LOUDNESS.indexOf(best)) best = state;
  }
  return best;
}

/** §9.3.2: who is waiting in one workspace, and how loudly — the desktop's per-workspace `unread` and `attention`. */
export function waitingInSession(state: Pick<SessionState, "agents">, sessionId: string): WaitingSummary {
  return summarizeWaiting(Object.values(state.agents).filter((agent) => agent.route.sessionId === sessionId));
}

/**
 * The colour a waiting state paints — the edge bar on both tabs and the
 * Workspaces tab's count. Red says blocked; done is a notification and takes
 * the bright green, as the desktop's `.workspace-state.done` and
 * `.agent-mark-badge.done` do (`--ok`, not the muted `--state-done`).
 */
export function waitingColor(state: WaitingState): string {
  return state === "blocked" ? colors.danger : colors.ok;
}

/**
 * The ink for text in a waiting state: `--danger-ink` for blocked (the
 * bar's `--danger` is 3.75:1 on `--chrome-bg`, too faint to read as words),
 * `--ok` for done, which already clears 7:1.
 */
export function waitingInk(state: WaitingState): string {
  return state === "blocked" ? colors.dangerInk : colors.ok;
}

/** §9.3.2: the desktop's "N agents waiting", as the row's chip. */
export function waitingLabel(count: number): string {
  return `${count} waiting`;
}

/** §9.3.2: "1 window" singular. */
export function windowCountLabel(count: number): string {
  return count === 1 ? "1 window" : `${count} windows`;
}

/** §9.3.1: the hooks hint shows when every adapter is notWired, partial, or absent. */
export function noAdapterWired(adapters: SessionState["adapters"]): boolean {
  return adapters.length > 0 && adapters.every((a) => a.hookWiring === "notWired" || a.hookWiring === "partial" || a.hookWiring === "absent");
}
