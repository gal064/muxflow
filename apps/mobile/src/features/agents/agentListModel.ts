// The Agents tab's row model (design.md §9.3.1), ported from the desktop's
// `apps/desktop/src/features/agents/agentsList.ts` and the agents section of
// `WorkspaceSidebar.tsx`. Pure: session state in, a flat list of headers,
// dividers and rows out, so a `FlatList` can draw it and vitest can cover it
// without a renderer.

import {
  agentPinned,
  agentWorkspaceName,
  displayState,
  isRecentIdle,
  recentExpirationMs,
  sortedAgents,
  waitingState,
  type AgentDisplayState,
  type WaitingState,
} from "../../store/selectors";
import type { Agent, SessionState } from "../../store/sessionStore";
import { agentTitle } from "./agentViews";

/**
 * The three orders the list can be in. `priority` is the desktop's `status`
 * sort — blocked, working, recent, idle, headed; `workspace` follows the
 * Workspaces tab, one group per workspace under the Pinned / Others dividers;
 * `pinned` is the priority order cut in two, the pinned rows above the rest.
 */
export type AgentListMode = "priority" | "workspace" | "pinned";

export const AGENT_LIST_MODES: readonly { mode: AgentListMode; label: string }[] = [
  { mode: "priority", label: "Priority" },
  { mode: "workspace", label: "Workspace" },
  { mode: "pinned", label: "Pinned" },
];

export function isAgentListMode(value: unknown): value is AgentListMode {
  return AGENT_LIST_MODES.some((entry) => entry.mode === value);
}

export type AgentPriorityBucket = "blocked" | "working" | "recent" | "idle";

/**
 * The four headings the priority order draws, in the order they are worth your
 * attention. `unknown` shares Idle rather than getting a fifth heading: it
 * means "nothing has told us what this is doing", a gap in reporting, not a
 * fifth thing an agent can be busy with. `state` is what the heading's own
 * mark draws — the group's, not any one row's.
 */
export const PRIORITY_SECTIONS: readonly { bucket: AgentPriorityBucket; label: string; state: AgentDisplayState }[] = [
  { bucket: "blocked", label: "Blocked", state: "blocked" },
  { bucket: "working", label: "Working", state: "working" },
  { bucket: "recent", label: "Recent", state: "idle" },
  { bucket: "idle", label: "Idle", state: "idle" },
];

/**
 * Which heading an agent sits under. Done shares Recent: acknowledging a
 * completion changes only the row's badge, so the tap that clears it does not
 * move the row out from under the finger. A gone agent is retained state,
 * not a demand — it cannot need you, so it sits under Idle whatever its last
 * lifecycle was (the desktop has no gone rows; `sortedAgents` already puts
 * them last).
 */
export function priorityBucket(agent: Agent, now: number): AgentPriorityBucket {
  if (!agent.present) return "idle";
  const state = displayState(agent);
  if (state === "blocked" || state === "working") return state;
  if (state === "done") return "recent";
  if (state === "idle" && isRecentIdle(agent, now)) return "recent";
  return "idle";
}

/**
 * The state docked to the adapter mark (the desktop's `AgentMark`): blocked
 * draws a red dot, working a spinner, done the attention badge, unknown a
 * dashed outline, idle nothing. A gone agent draws nothing — quiet is the
 * resting state, and its retained lifecycle is not a live report.
 */
export function markState(agent: Agent): AgentDisplayState {
  return agent.present ? displayState(agent) : "idle";
}

/**
 * When the next Recent row becomes Idle with no event at all, or undefined
 * when none will. The desktop's `useRecentIdleClock` timer target.
 */
export function nextRecentExpiration(agents: readonly Agent[], now: number): number | undefined {
  let next: number | undefined;
  for (const agent of agents) {
    if (!agent.present) continue;
    const expiration = recentExpirationMs(agent);
    if (expiration === undefined || expiration <= now) continue;
    next = next === undefined ? expiration : Math.min(next, expiration);
  }
  return next;
}

export interface AgentRowItem {
  kind: "agent";
  key: string;
  agent: Agent;
  /** What the mark's badge draws. */
  state: AgentDisplayState;
  /**
   * Blocked, or finished and not yet looked at: the row's edge bar, in that
   * state's colour (the desktop's unread "1" badge, `needsAttention(state)`).
   */
  waiting: WaitingState | undefined;
  /** The window (tab) is pinned: a pin after the title, the desktop's row pin. */
  windowPinned: boolean;
  /**
   * The workspace is pinned. No row draws it — a second pin beside the
   * workspace's name read as a second pinned thing — but Pinned mode sorts
   * by it, and the spoken label names it.
   */
  workspacePinned: boolean;
  /** The tab's name (`agentTitle`): the adapter is the icon's job. */
  title: string;
  /** The workspace, or nothing where the heading already names it (Workspace mode). */
  subtitle: string | undefined;
}

export interface PrioritySectionItem {
  kind: "section";
  key: string;
  bucket: AgentPriorityBucket;
  label: string;
  state: AgentDisplayState;
  count: number;
}

export interface WorkspaceGroupItem {
  kind: "group";
  key: string;
  sessionId: string;
  workspaceName: string;
  pinned: boolean;
  count: number;
}

export interface DividerItem {
  kind: "divider";
  key: string;
  label: string;
}

export type AgentListItem = AgentRowItem | PrioritySectionItem | WorkspaceGroupItem | DividerItem;

type ListState = Pick<SessionState, "agents" | "sessions" | "windows" | "adapters">;

export function buildAgentListItems(state: ListState, mode: AgentListMode, now = Date.now()): AgentListItem[] {
  switch (mode) {
    case "workspace": return workspaceItems(state);
    case "pinned": return pinnedItems(state, now);
    case "priority": return priorityItems(state, now);
  }
}

/**
 * One row. Line 1 is the tab's name and line 2 the workspace's — the desktop
 * row's `agent-session-label` over `agent-detail`; the adapter shows only in
 * the icon. Workspace mode's rows sit under a heading that already names the
 * workspace, so they get no line 2 at all.
 */
function agentRow(state: ListState, agent: Agent, subtitle: "workspace" | "none"): AgentRowItem {
  return {
    kind: "agent",
    key: `agent:${agent.id}`,
    agent,
    state: markState(agent),
    waiting: waitingState(agent),
    windowPinned: Boolean(state.windows[agent.route.windowId]?.pinned),
    workspacePinned: Boolean(state.sessions[agent.route.sessionId]?.pinned),
    title: agentTitle(state, agent),
    subtitle: subtitle === "workspace" ? agentWorkspaceName(state, agent) : undefined,
  };
}

/** Buckets `sortedAgents`' order without re-sorting it, so the two agree. */
function priorityItems(state: ListState, now: number): AgentListItem[] {
  const agents = sortedAgents(state, now);
  const items: AgentListItem[] = [];
  for (const section of PRIORITY_SECTIONS) {
    const rows = agents.filter((agent) => priorityBucket(agent, now) === section.bucket);
    if (rows.length === 0) continue;
    items.push({ kind: "section", key: `section:${section.bucket}`, bucket: section.bucket, label: section.label, state: section.state, count: rows.length });
    for (const agent of rows) items.push(agentRow(state, agent, "workspace"));
  }
  return items;
}

/**
 * The priority order cut in two: every row whose window or workspace is
 * pinned (`agentPinned`, the same predicate that leads pinned rows inside a
 * priority heading) under a Pinned divider, the rest under Unpinned. Neither
 * block has headings inside it — what is working still comes first, then
 * what has a dot, then the rest, exactly as `sortedAgents` orders them — and
 * an empty block is left out rather than drawn as a divider over nothing.
 */
function pinnedItems(state: ListState, now: number): AgentListItem[] {
  const agents = sortedAgents(state, now);
  const blocks = [
    { key: "pinned", label: "Pinned", agents: agents.filter((agent) => agentPinned(state, agent)) },
    { key: "unpinned", label: "Unpinned", agents: agents.filter((agent) => !agentPinned(state, agent)) },
  ];
  const items: AgentListItem[] = [];
  for (const block of blocks) {
    if (block.agents.length === 0) continue;
    items.push({ kind: "divider", key: `divider:${block.key}`, label: block.label });
    for (const agent of block.agents) items.push(agentRow(state, agent, "workspace"));
  }
  return items;
}

/**
 * The desktop's `byWorkspace`: the Workspaces tab's order (pinned first, then
 * host order), then inside one workspace a pinned window's agents lead, then
 * window index, then name. Present agents lead their gone peers inside a
 * workspace so retained rows sink to the bottom of their group.
 */
function compareByWorkspace(state: ListState, left: Agent, right: Agent): number {
  const leftSession = state.sessions[left.route.sessionId];
  const rightSession = state.sessions[right.route.sessionId];
  const leftWindow = state.windows[left.route.windowId];
  const rightWindow = state.windows[right.route.windowId];
  return Number(Boolean(rightSession?.pinned)) - Number(Boolean(leftSession?.pinned))
    || (leftSession?.order ?? Number.MAX_SAFE_INTEGER) - (rightSession?.order ?? Number.MAX_SAFE_INTEGER)
    || agentWorkspaceName(state, left).localeCompare(agentWorkspaceName(state, right))
    // Keep a workspace contiguous even when two same-named ones share an order.
    || left.route.sessionId.localeCompare(right.route.sessionId)
    || Number(right.present) - Number(left.present)
    || Number(Boolean(rightWindow?.pinned)) - Number(Boolean(leftWindow?.pinned))
    || (leftWindow?.index ?? Number.MAX_SAFE_INTEGER) - (rightWindow?.index ?? Number.MAX_SAFE_INTEGER)
    || left.displayName.localeCompare(right.displayName)
    || left.id.localeCompare(right.id);
}

interface WorkspaceGroup {
  sessionId: string;
  workspaceName: string;
  pinned: boolean;
  agents: Agent[];
}

/** Groups an already workspace-ordered list without re-sorting its rows. */
function groupByWorkspace(state: ListState, agents: readonly Agent[]): WorkspaceGroup[] {
  const groups = new Map<string, WorkspaceGroup>();
  for (const agent of agents) {
    const sessionId = agent.route.sessionId;
    const existing = groups.get(sessionId);
    if (existing) existing.agents.push(agent);
    else groups.set(sessionId, {
      sessionId,
      workspaceName: agentWorkspaceName(state, agent),
      pinned: Boolean(state.sessions[sessionId]?.pinned),
      agents: [agent],
    });
  }
  return [...groups.values()];
}

/**
 * The sidebar's two dividers over the per-workspace groups, or neither when
 * no workspace is pinned: a pinned workspace's agents read as one block above
 * the rest rather than as a mark repeated on every heading.
 */
function workspaceItems(state: ListState): AgentListItem[] {
  const agents = Object.values(state.agents).sort((left, right) => compareByWorkspace(state, left, right));
  const groups = groupByWorkspace(state, agents);
  const blocks = groups.some((group) => group.pinned)
    ? [
      { key: "pinned", label: "Pinned", groups: groups.filter((group) => group.pinned) },
      { key: "others", label: "Others", groups: groups.filter((group) => !group.pinned) },
    ].filter((block) => block.groups.length > 0)
    : [{ key: "all", label: undefined, groups }];
  const items: AgentListItem[] = [];
  for (const block of blocks) {
    if (block.label) items.push({ kind: "divider", key: `divider:${block.key}`, label: block.label });
    for (const group of block.groups) {
      items.push({ kind: "group", key: `group:${group.sessionId}`, sessionId: group.sessionId, workspaceName: group.workspaceName, pinned: group.pinned, count: group.agents.length });
      for (const agent of group.agents) items.push(agentRow(state, agent, "none"));
    }
  }
  return items;
}
