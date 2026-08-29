import { compareAgents, displayState } from "./selectors";
import type { AgentDisplayState, AgentRecord } from "./types";

/**
 * The sidebar's agents section is a flat list across every workspace, with one
 * control: the order it is in.
 *
 * `workspace` follows the workspace list above it, so the two halves of the
 * sidebar read as one thing. `status` is the visual queue: blocked, working,
 * done, recently idle, then older idle. Rows inside each state follow their
 * last real lifecycle transition rather than every hook update.
 *
 * The two modes were called `grouped` and `priority`, which named neither the
 * thing sorted nor — in `grouped`'s case — what it does, since neither mode
 * has ever drawn a group heading. The orders themselves did not change; the
 * button now says what each one is. Persisted values from before the rename
 * migrate in `features/shell/types.ts`.
 */
export type AgentSortMode = "status" | "workspace";

/** How long an acknowledged completed agent stays near the active work. */
export const RECENT_IDLE_WINDOW_MILLIS = 4 * 60 * 60 * 1_000;

export type AgentPriorityBucket = "blocked" | "working" | "done" | "recent" | "idle";

export function isAgentSortMode(value: unknown): value is AgentSortMode {
  return value === "status" || value === "workspace";
}

export function nextSortMode(mode: AgentSortMode): AgentSortMode {
  return mode === "workspace" ? "status" : "workspace";
}

/**
 * What the toggle says, which is not what the mode is called.
 *
 * The persisted value stays `status` — it is in the app-state contract and two
 * migrations already point at it — but "status" names the field the rows are
 * keyed on rather than what the mode does for you, and the mode draws headings
 * now: Blocked, Working, Done, Recent, Idle, in the order you should deal with them.
 * That is a priority, so the button says priority.
 */
export function sortModeLabel(mode: AgentSortMode): string {
  return mode === "status" ? "priority" : "workspace";
}

/** Where a row sits in the workspace list and in its workspace's tab strip. */
export interface AgentLocation {
  workspaceOrder: number;
  workspaceName: string;
  /** User-facing host identity, needed when workspace names collide. */
  hostLabel?: string;
  tabIndex?: number;
  /**
   * Whether this agent's workspace and tab are pinned, as the host reports
   * them.
   *
   * `workspaceOrder` already puts a pinned workspace's rows first in the
   * workspace ordering, because it comes from the sidebar list that is itself
   * pinned-first. These two are what the *priority* ordering needs, where there
   * is no workspace ranking to inherit.
   */
  workspacePinned?: boolean;
  tabPinned?: boolean;
}

export interface AgentListRow {
  agent: AgentRecord;
  state: AgentDisplayState;
  /** The priority heading this row belongs under; Recent still draws Idle. */
  priorityBucket: AgentPriorityBucket;
  location: AgentLocation;
  /** A row is only clickable when it resolves to an exact live pane. */
  routable: boolean;
  /** Whether this agent's tab is pinned. Workspace pins order workspaces. */
  pinned: boolean;
}

export interface AgentWorkspaceGroup {
  key: string;
  workspaceName: string;
  hostLabel: string;
  /**
   * Whether this group's workspace is pinned — which of the two dividers the
   * group sits under. Read from the rows rather than passed in: every row in a
   * group shares one workspace, so they all carry the same answer.
   */
  pinned: boolean;
  rows: AgentListRow[];
}

/**
 * Groups an already workspace-ordered list without re-sorting its rows. The
 * server identity is part of the key because a reconnect can reuse a profile
 * and session id while still referring to a different tmux server.
 */
export function groupAgentRows(rows: readonly AgentListRow[]): AgentWorkspaceGroup[] {
  const groups = new Map<string, AgentWorkspaceGroup>();
  for (const row of rows) {
    const hostLabel = row.location.hostLabel || row.agent.hostProfileId || "unknown host";
    const key = [row.agent.hostProfileId, row.agent.serverIdentity, row.agent.sessionId].join("\0");
    const existing = groups.get(key);
    if (existing) existing.rows.push(row);
    else groups.set(key, {
      key,
      workspaceName: row.location.workspaceName,
      hostLabel,
      pinned: Boolean(row.location.workspacePinned),
      rows: [row],
    });
  }
  return [...groups.values()];
}

/**
 * The five buckets the priority order draws, in the order they are worth your
 * attention.
 *
 * `unknown` shares Idle's bucket rather than getting a fifth heading: it means
 * "nothing has told us what this is doing", which is a gap in reporting and
 * not a fifth thing an agent can be busy with. A heading per reporting gap
 * would put the least informative group on equal footing with Blocked.
 */
export const AGENT_STATUS_GROUPS = [
  { key: "blocked", label: "Blocked", state: "blocked" },
  { key: "working", label: "Working", state: "working" },
  { key: "done", label: "Done", state: "done" },
  { key: "recent", label: "Recent", state: "idle" },
  { key: "idle", label: "Idle", state: "idle" },
] as const satisfies readonly { key: AgentPriorityBucket; label: string; state: AgentDisplayState }[];

const PRIORITY_RANK = new Map<AgentPriorityBucket, number>(
  AGENT_STATUS_GROUPS.map((group, index) => [group.key, index]),
);

export interface AgentStatusGroup {
  key: string;
  label: string;
  /** The state the heading's dot draws — the group's own, not any one row's. */
  state: AgentDisplayState;
  rows: AgentListRow[];
}

/**
 * Buckets an already status-ordered list without re-sorting its rows.
 *
 * `buildAgentRows` assigns the bucket once and sorts with the same bucket
 * ranking, so the flat keyboard order and the grouped reading order agree.
 */
export function groupAgentRowsByStatus(rows: readonly AgentListRow[]): AgentStatusGroup[] {
  return AGENT_STATUS_GROUPS
    .map((group) => ({
      key: group.key,
      label: group.label,
      state: group.state,
      rows: rows.filter((row) => row.priorityBucket === group.key),
    }))
    .filter((group) => group.rows.length > 0);
}

/**
 * True when the agent is asking for a human: blocked, or finished without its
 * pane having been looked at. These are the rows ⌘⇧U jumps to and the ones
 * that carry an unread badge.
 */
export function needsAttention(state: AgentDisplayState): boolean {
  return state === "blocked" || state === "done";
}

export function buildAgentRows(
  agents: readonly AgentRecord[],
  locate: (agent: AgentRecord) => AgentLocation,
  routable: (agent: AgentRecord) => boolean,
  mode: AgentSortMode,
  now = Date.now(),
): AgentListRow[] {
  const rows = agents.map((agent) => {
    const location = locate(agent);
    const state = displayState(agent);
    return {
      agent,
      state,
      priorityBucket: priorityBucket(state, agent.lifecycleChangedAt, now),
      location,
      routable: routable(agent),
      pinned: Boolean(location.tabPinned),
    };
  });
  return rows.sort(mode === "status" ? byStatus : byWorkspace);
}

function byStatus(left: AgentListRow, right: AgentListRow): number {
  // Status remains the primary queue. A tab pin leads only its own status;
  // inside pinned and unpinned peers, only a real lifecycle transition changes
  // recency, so repeated hooks and route-only updates cannot reshuffle rows.
  return (PRIORITY_RANK.get(left.priorityBucket) ?? Number.MAX_SAFE_INTEGER)
      - (PRIORITY_RANK.get(right.priorityBucket) ?? Number.MAX_SAFE_INTEGER)
    || Number(right.pinned) - Number(left.pinned)
    || right.agent.lifecycleChangedAt - left.agent.lifecycleChangedAt
    || left.agent.id.localeCompare(right.agent.id);
}

function priorityBucket(
  state: AgentDisplayState,
  lifecycleChangedAt: number,
  now: number,
): AgentPriorityBucket {
  if (state === "blocked" || state === "working" || state === "done") return state;
  if (state === "idle" && now - lifecycleChangedAt < RECENT_IDLE_WINDOW_MILLIS) return "recent";
  return "idle";
}

function byWorkspace(left: AgentListRow, right: AgentListRow): number {
  return left.location.workspaceOrder - right.location.workspaceOrder
    || left.location.workspaceName.localeCompare(right.location.workspaceName)
    // Keep a workspace contiguous even when same-named workspaces on two
    // hosts happen to have the same display order and tab indexes.
    || left.agent.hostProfileId.localeCompare(right.agent.hostProfileId)
    || left.agent.serverIdentity.localeCompare(right.agent.serverIdentity)
    || left.agent.sessionId.localeCompare(right.agent.sessionId)
    // Inside one workspace, a pinned tab's agents lead — the strip's own order,
    // which is what this mode exists to follow.
    || Number(Boolean(right.location.tabPinned)) - Number(Boolean(left.location.tabPinned))
    || (left.location.tabIndex ?? Number.MAX_SAFE_INTEGER) - (right.location.tabIndex ?? Number.MAX_SAFE_INTEGER)
    || left.agent.displayName.localeCompare(right.agent.displayName)
    || left.agent.id.localeCompare(right.agent.id);
}

/**
 * The row ⌘⇧U and the titlebar bell go to: the loudest row that actually wants
 * attention and can actually be reached. Jumping to an idle agent because it
 * happened to sort first would make the shortcut useless.
 *
 * Order: blocked first, then unread completed; ties break on most recently
 * updated. Deliberately `compareAgents` rather than the list's own `byStatus`:
 * pins organize peers inside a status, while this control answers only which
 * reachable agent needs attention most.
 */
export function jumpTarget(rows: readonly AgentListRow[]): AgentListRow | undefined {
  return [...rows].sort((left, right) => compareAgents(left.agent, right.agent))
    .find((row) => row.routable && needsAttention(row.state));
}

/** How many rows are waiting on a human — the number on the titlebar's bell. */
export function unreadCount(rows: readonly AgentListRow[]): number {
  return rows.filter((row) => needsAttention(row.state)).length;
}
