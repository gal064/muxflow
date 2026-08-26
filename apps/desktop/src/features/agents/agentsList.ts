import { compareAgents, displayState } from "./selectors";
import { pinRank } from "../shell/pins";
import type { AgentDisplayState, AgentRecord } from "./types";

/**
 * The sidebar's agents section is a flat list across every workspace, with one
 * control: the order it is in.
 *
 * `workspace` follows the workspace list above it, so the two halves of the
 * sidebar read as one thing. `status` is the inbox: blocked first, then
 * done-but-unread, then working, then idle — Herdr's ranking, in which a
 * finished agent outranks a running one because a finished agent is the one
 * waiting on a human.
 *
 * The two modes were called `grouped` and `priority`, which named neither the
 * thing sorted nor — in `grouped`'s case — what it does, since neither mode
 * has ever drawn a group heading. The orders themselves did not change; the
 * button now says what each one is. Persisted values from before the rename
 * migrate in `features/shell/types.ts`.
 */
export type AgentSortMode = "status" | "workspace";

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
 * now: Blocked, Working, Done, Idle, in the order you should deal with them.
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
   * When this agent's workspace and tab were pinned, if either was.
   *
   * `workspaceOrder` already puts a pinned workspace's rows first in the
   * workspace ordering, because it comes from the sidebar list that is itself
   * pinned-first. These two are what the *priority* ordering needs, where there
   * is no workspace ranking to inherit.
   */
  workspacePinnedAt?: number;
  tabPinnedAt?: number;
}

export interface AgentListRow {
  agent: AgentRecord;
  state: AgentDisplayState;
  location: AgentLocation;
  /** A row is only clickable when it resolves to an exact live pane. */
  routable: boolean;
  /** In the leading block: its workspace is pinned, or its tab is. */
  pinned: boolean;
}

export interface AgentWorkspaceGroup {
  key: string;
  workspaceName: string;
  hostLabel: string;
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
    else groups.set(key, { key, workspaceName: row.location.workspaceName, hostLabel, rows: [row] });
  }
  return [...groups.values()];
}

/**
 * The four buckets the priority order draws, in the order they are worth your
 * attention.
 *
 * `unknown` shares Idle's bucket rather than getting a fifth heading: it means
 * "nothing has told us what this is doing", which is a gap in reporting and
 * not a fifth thing an agent can be busy with. A heading per reporting gap
 * would put the least informative group on equal footing with Blocked.
 */
export const AGENT_STATUS_GROUPS = [
  { key: "blocked", label: "Blocked", states: ["blocked"] },
  { key: "working", label: "Working", states: ["working"] },
  { key: "done", label: "Done", states: ["done"] },
  { key: "idle", label: "Idle", states: ["idle", "unknown"] },
] as const satisfies readonly { key: string; label: string; states: readonly AgentDisplayState[] }[];

export interface AgentStatusGroup {
  key: string;
  label: string;
  /**
   * The state the heading's dot draws — the group's own, not any one row's.
   * Absent on the pinned block, which is not a state and draws a pin instead.
   */
  state?: AgentDisplayState;
  rows: AgentListRow[];
}

/**
 * Buckets an already status-ordered list without re-sorting its rows.
 *
 * The bucket order is not the sort order: `compareAgents` ranks done-unread
 * above working, because a finished agent is the one waiting on a human. That
 * is right for "which single row does ⌘⇧U jump to" and wrong for a column you
 * read top to bottom, where Working sitting between Blocked and Done is what
 * makes the list scan as a queue. Rows keep their sorted order inside each
 * bucket, so the loudest row in a group is still its first.
 */
export function groupAgentRowsByStatus(rows: readonly AgentListRow[]): AgentStatusGroup[] {
  // The pinned block is lifted out above the headings rather than sorted to the
  // top of whichever bucket each of its rows lands in. Left in the buckets, a
  // pinned agent that happened to be idle would sit under an Idle heading below
  // three other headings — first in its group, and nowhere near first in the
  // list, which is the one thing a pin promises.
  const pinned = rows.filter((row) => row.pinned);
  const buckets = AGENT_STATUS_GROUPS
    .map((group) => ({
      key: group.key,
      label: group.label,
      state: group.states[0] as AgentDisplayState,
      rows: rows.filter((row) => !row.pinned && (group.states as readonly AgentDisplayState[]).includes(row.state)),
    }))
    .filter((group) => group.rows.length > 0);
  return pinned.length > 0 ? [{ key: "pinned", label: "Pinned", rows: pinned }, ...buckets] : buckets;
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
): AgentListRow[] {
  const rows = agents.map((agent) => {
    const location = locate(agent);
    return {
      agent,
      state: displayState(agent),
      location,
      routable: routable(agent),
      pinned: location.workspacePinnedAt !== undefined || location.tabPinnedAt !== undefined,
    };
  });
  return rows.sort(mode === "status" ? byStatus : byWorkspace);
}

function byStatus(left: AgentListRow, right: AgentListRow): number {
  // The pinned block first, in workspace pin order and then tab pin order, and
  // compareAgents — blocked > done-unread > working > unknown > idle, then
  // most-recently-updated — inside each block. Reusing it keeps one definition
  // of "loudest"; the pin keys only decide which block a row is in.
  return Number(right.pinned) - Number(left.pinned)
    || pinRank(left.location.workspacePinnedAt) - pinRank(right.location.workspacePinnedAt)
    || pinRank(left.location.tabPinnedAt) - pinRank(right.location.tabPinnedAt)
    || compareAgents(left.agent, right.agent);
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
    || pinRank(left.location.tabPinnedAt) - pinRank(right.location.tabPinnedAt)
    || (left.location.tabIndex ?? Number.MAX_SAFE_INTEGER) - (right.location.tabIndex ?? Number.MAX_SAFE_INTEGER)
    || left.agent.displayName.localeCompare(right.agent.displayName)
    || left.agent.id.localeCompare(right.agent.id);
}

/**
 * The row ⌘⇧U and the titlebar bell go to: the top of the status order,
 * restricted to rows that actually want attention and can actually be reached.
 * Jumping to an idle agent because it happened to sort first would make the
 * shortcut useless.
 *
 * Order: blocked first, then unread completed; ties break on most recently
 * updated.
 */
export function jumpTarget(rows: readonly AgentListRow[]): AgentListRow | undefined {
  return [...rows].sort(byStatus).find((row) => row.routable && needsAttention(row.state));
}

/** How many rows are waiting on a human — the number on the titlebar's bell. */
export function unreadCount(rows: readonly AgentListRow[]): number {
  return rows.filter((row) => needsAttention(row.state)).length;
}
