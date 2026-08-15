import { compareAgents, displayState } from "./selectors";
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

/** Where a row sits in the workspace list and in its workspace's tab strip. */
export interface AgentLocation {
  workspaceOrder: number;
  workspaceName: string;
  tabIndex?: number;
}

export interface AgentListRow {
  agent: AgentRecord;
  state: AgentDisplayState;
  location: AgentLocation;
  /** A row is only clickable when it resolves to an exact live pane. */
  routable: boolean;
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
  const rows = agents.map((agent) => ({
    agent,
    state: displayState(agent),
    location: locate(agent),
    routable: routable(agent),
  }));
  return rows.sort(mode === "status" ? byStatus : byWorkspace);
}

function byStatus(left: AgentListRow, right: AgentListRow): number {
  // compareAgents is already blocked > done-unread > working > unknown > idle,
  // then most-recently-updated. Reusing it keeps one definition of "loudest".
  return compareAgents(left.agent, right.agent);
}

function byWorkspace(left: AgentListRow, right: AgentListRow): number {
  return left.location.workspaceOrder - right.location.workspaceOrder
    || left.location.workspaceName.localeCompare(right.location.workspaceName)
    || (left.location.tabIndex ?? Number.MAX_SAFE_INTEGER) - (right.location.tabIndex ?? Number.MAX_SAFE_INTEGER)
    || left.agent.displayName.localeCompare(right.agent.displayName)
    || left.agent.id.localeCompare(right.agent.id);
}

/**
 * The row ⌘⇧U goes to: the top of the status order, restricted to rows that
 * actually want attention and can actually be reached. Jumping to an idle agent
 * because it happened to sort first would make the shortcut useless.
 */
export function jumpTarget(rows: readonly AgentListRow[]): AgentListRow | undefined {
  return [...rows].sort(byStatus).find((row) => row.routable && needsAttention(row.state));
}

/** How many rows are waiting on a human — the number on the titlebar's bell. */
export function unreadCount(rows: readonly AgentListRow[]): number {
  return rows.filter((row) => needsAttention(row.state)).length;
}
