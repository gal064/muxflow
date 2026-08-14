import type { Pane, Session, TmuxSnapshot, Window } from "../../app/types";
import { displayState } from "../agents/selectors";
import { needsAttention } from "../agents/agentsList";
import type { AgentAttentionRollup, AgentDisplayState, AgentRecord } from "../agents/types";
import { orderedSessions } from "../shell/model";

/**
 * A workspace row in the sidebar: name, what its loudest agent is doing, and
 * where it is on disk.
 *
 * The row is derived, not stored. tmux owns the topology and the agent store
 * owns the states, so this module's whole job is to answer "what does one
 * sidebar row say" from those two, with no per-row state to fall out of sync.
 */
export interface WorkspaceRowModel {
  session: Session;
  active: boolean;
  /** Aggregate of the workspace's loudest agent, per the brief. */
  attention: AgentAttentionRollup["state"];
  /** Number of that workspace's agents waiting on a human. */
  unread: number;
  working: boolean;
  /** Last thing an agent here was seen doing; empty when there are none. */
  activity?: string;
  /** `branch* · ~/dev/thing`, or just the path when the branch is unknown. */
  metadata?: string;
}

export interface WorkspaceRowInputs {
  snapshot: TmuxSnapshot;
  activeSessionId?: string;
  agents: readonly AgentRecord[];
  attentionByWorkspace: ReadonlyMap<string, AgentAttentionRollup>;
  /** Branch for the active workspace only; nothing else has a git snapshot. */
  activeBranch?: string;
  /** Home directory, so paths render the way a shell prompt would. */
  home?: string;
}

export function workspaceRows(inputs: WorkspaceRowInputs): WorkspaceRowModel[] {
  const loudest = loudestAgentBySession(inputs.agents);
  const unreadBySession = new Map<string, number>();
  for (const agent of inputs.agents) {
    if (!needsAttention(displayState(agent))) continue;
    unreadBySession.set(agent.sessionId, (unreadBySession.get(agent.sessionId) ?? 0) + 1);
  }
  return orderedSessions(inputs.snapshot.sessions).map((session) => {
    const attention = inputs.attentionByWorkspace.get(session.id)?.state ?? "none";
    const agent = loudest.get(session.id);
    const active = session.id === inputs.activeSessionId;
    return {
      session,
      active,
      attention,
      unread: unreadBySession.get(session.id) ?? 0,
      working: attention === "working",
      activity: agent ? `${agent.displayName} · ${activityWord(displayState(agent))}` : undefined,
      metadata: metadataLine(sessionPath(inputs.snapshot, session.id), active ? inputs.activeBranch : undefined, inputs.home),
    };
  });
}

/** The one agent whose state the workspace row inherits. */
function loudestAgentBySession(agents: readonly AgentRecord[]): Map<string, AgentRecord> {
  const rank: Record<AgentDisplayState, number> = { blocked: 4, done: 3, working: 2, unknown: 1, idle: 0 };
  const loudest = new Map<string, AgentRecord>();
  for (const agent of agents) {
    const current = loudest.get(agent.sessionId);
    if (!current
      || rank[displayState(agent)] > rank[displayState(current)]
      || (rank[displayState(agent)] === rank[displayState(current)] && agent.updatedAt > current.updatedAt)) {
      loudest.set(agent.sessionId, agent);
    }
  }
  return loudest;
}

function activityWord(state: AgentDisplayState): string {
  return state === "done" ? "done, unread" : state;
}

/** The active pane of the workspace's active window — the shell's own cwd. */
export function sessionPath(snapshot: TmuxSnapshot, sessionId: string): string | undefined {
  const windows = snapshot.windows.filter((item: Window) => item.sessionId === sessionId);
  const window = windows.find((item) => item.active) ?? windows.sort((left, right) => left.index - right.index)[0];
  if (!window) return undefined;
  const panes = snapshot.panes.filter((pane: Pane) => pane.windowId === window.id);
  const pane = panes.find((item) => item.active) ?? panes.sort((left, right) => left.index - right.index)[0];
  return pane?.currentPath || undefined;
}

export function metadataLine(path: string | undefined, branch: string | undefined, home: string | undefined): string | undefined {
  const shortened = abbreviateHome(path, home);
  if (branch && shortened) return `${branch} · ${shortened}`;
  return branch ?? shortened;
}

const HOME_SHAPE = /^(\/home\/[^/]+|\/Users\/[^/]+|\/root)(?=\/|$)/u;

/**
 * The tmux user's home directory, inferred from where its panes actually are.
 *
 * The app cannot ask: the local machine's home is not the remote host's, and
 * finding out for real would need a protocol change this phase is not allowed
 * to make. So it takes the home-shaped prefix that the most panes share, which
 * is the tmux user's home on any normal host. When panes disagree, or none
 * looks like a home directory, it returns nothing and paths render in full —
 * a long path is a worse row than a short one, but a wrong `~` is a lie.
 */
export function inferHome(paths: readonly (string | undefined)[]): string | undefined {
  const counts = new Map<string, number>();
  for (const path of paths) {
    const prefix = path ? HOME_SHAPE.exec(path)?.[1] : undefined;
    if (prefix) counts.set(prefix, (counts.get(prefix) ?? 0) + 1);
  }
  let best: string | undefined;
  let bestCount = 0;
  for (const [prefix, count] of counts) {
    if (count > bestCount || (count === bestCount && best !== undefined && prefix < best)) {
      best = prefix;
      bestCount = count;
    }
  }
  return best;
}

export function abbreviateHome(path: string | undefined, home: string | undefined): string | undefined {
  if (!path) return undefined;
  if (!home || home === "/" || !path.startsWith(home)) return path;
  const rest = path.slice(home.length);
  return rest === "" ? "~" : rest.startsWith("/") ? `~${rest}` : path;
}
