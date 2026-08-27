import type { Pane, Session, TmuxSnapshot, Window } from "../../app/types";
import { compareAgents, displayState } from "../agents/selectors";
import { needsAttention } from "../agents/agentsList";
import { agentSessionLabel } from "../agents/agentLabels";
import type { AgentAdapterDescriptor, AgentAdapterId, AgentAttentionRollup, AgentDisplayState, AgentRecord } from "../agents/types";
import { orderedSessions } from "../shell/model";
import { pinnedFirst } from "../shell/pins";

/**
 * A workspace row in the sidebar: name and what its loudest few agents are
 * doing.
 *
 * The row is derived, not stored. tmux owns the topology and the agent store
 * owns the states, so this module's whole job is to answer "what does one
 * sidebar row say" from those two, with no per-row state to fall out of sync.
 *
 * `branch` and `path` are carried for ⌘P alone — the sidebar renders neither.
 * A workspace holds many tabs in many directories, so one branch per row was
 * a lie half the time; in the switcher they are match keys, where searching
 * for `~/dev/thing` or a branch name is the fastest way to a workspace.
 */
export interface WorkspaceRowModel {
  session: Session;
  active: boolean;
  /** Aggregate of the workspace's loudest agent, per the brief. */
  attention: AgentAttentionRollup["state"];
  /** Number of that workspace's agents waiting on a human. */
  unread: number;
  /** Shift-clicked to the top of the list; the row draws a pin. */
  pinned: boolean;
  working: boolean;
  /** The loudest agents here, at most {@link WORKSPACE_ROW_AGENT_LIMIT}. */
  agents: WorkspaceRowAgent[];
  /** How many agents this workspace has beyond the ones the row lists. */
  agentOverflow: number;
  /** Branch of the active workspace; nothing else has a git snapshot. */
  branch?: string;
  /** The shell's own cwd, abbreviated the way a prompt would write it. */
  path?: string;
}

/** One agent line on a workspace row. */
export interface WorkspaceRowAgent {
  id: string;
  adapterId: AgentAdapterId;
  name: string;
  state: AgentDisplayState;
}

/**
 * How many agent lines a row shows before it stops listing and starts
 * counting. Three is what fits under a row title without the sidebar turning
 * into the agents list that already sits below it.
 */
export const WORKSPACE_ROW_AGENT_LIMIT = 3;

export interface WorkspaceRowInputs {
  snapshot: TmuxSnapshot;
  activeSessionId?: string;
  agents: readonly AgentRecord[];
  adapters?: readonly AgentAdapterDescriptor[];
  attentionByWorkspace: ReadonlyMap<string, AgentAttentionRollup>;
  /** Branch for the active workspace only; nothing else has a git snapshot. */
  activeBranch?: string;
  /** Home directory, so paths render the way a shell prompt would. */
  home?: string;
  /**
   * The sidebar's one filter: only pinned workspaces get a row.
   *
   * Applied here rather than by each consumer, because the row list is also
   * what ⌘1–9, the ⌘P switcher and the agents list's workspace order are built
   * from — a filter the sidebar applied on its own would be a list only the
   * sidebar agreed with. The workspace on screen is the one exception: it keeps
   * its row until the user selects another, so the filter can never hide what
   * the shell is currently showing.
   */
  pinnedOnly?: boolean;
  /**
   * When each pinned workspace on this server was pinned.
   *
   * Applied here rather than in the sidebar because this list *is* the
   * workspace order: ⌘1–9, the ⌘P switcher and the agents list's workspace
   * ranking all read it, and a pin the sidebar applied on its own would be a
   * pin only the sidebar knew about.
   */
  pinnedAt?: ReadonlyMap<string, number>;
}

export function workspaceRows(inputs: WorkspaceRowInputs): WorkspaceRowModel[] {
  const loudest = topAgentsBySession(inputs.agents);
  const unreadBySession = new Map<string, number>();
  for (const agent of inputs.agents) {
    if (!needsAttention(displayState(agent))) continue;
    unreadBySession.set(agent.sessionId, (unreadBySession.get(agent.sessionId) ?? 0) + 1);
  }
  const ordered = pinnedFirst(
    orderedSessions(inputs.snapshot.sessions).filter((session) => !inputs.pinnedOnly
      || inputs.pinnedAt?.has(session.id)
      || session.id === inputs.activeSessionId),
    (session) => inputs.pinnedAt?.get(session.id),
  );
  return ordered.map((session) => {
    const rollup = inputs.attentionByWorkspace.get(session.id);
    const attention = rollup?.state ?? "none";
    const here = loudest.get(session.id);
    const shown = here?.top ?? [];
    const active = session.id === inputs.activeSessionId;
    return {
      session,
      active,
      attention,
      unread: unreadBySession.get(session.id) ?? 0,
      pinned: inputs.pinnedAt?.has(session.id) ?? false,
      working: (rollup?.working ?? 0) > 0,
      agents: shown.map((agent) => ({
        id: agent.id,
        adapterId: agent.adapterId,
        name: agentSessionLabel(agent, inputs.adapters ?? []),
        state: displayState(agent),
      })),
      agentOverflow: (here?.total ?? 0) - shown.length,
      branch: active ? inputs.activeBranch : undefined,
      path: abbreviateHome(sessionPath(inputs.snapshot, session.id), inputs.home),
    };
  });
}

/**
 * The agents a workspace row inherits its state from, loudest first.
 *
 * `compareAgents` and nothing else. It is the same ranking the agents list
 * below the sidebar sorts by, and a second copy of it here would mean two
 * agents with the same state and the same update time appearing in one order
 * on the workspace row and another in the list directly beneath it — same
 * data, two answers, adjacent on screen.
 */
function topAgentsBySession(
  agents: readonly AgentRecord[],
): Map<string, { top: AgentRecord[]; total: number }> {
  const loudest = new Map<string, { top: AgentRecord[]; total: number }>();
  // Sorted once, then grouped: a stable partition of an ordered list leaves
  // every group ordered, so no group needs sorting again.
  for (const agent of [...agents].sort(compareAgents)) {
    const here = loudest.get(agent.sessionId);
    if (!here) {
      loudest.set(agent.sessionId, { top: [agent], total: 1 });
      continue;
    }
    here.total += 1;
    if (here.top.length < WORKSPACE_ROW_AGENT_LIMIT) here.top.push(agent);
  }
  return loudest;
}

/**
 * Branch and path recomposed, for ⌘P — the one surface that shows either.
 *
 * The switcher matches on the pair and shows what it matched. Composing that
 * here keeps the separator in the module that owns the fields, rather than in
 * a component that would rebuild it on every keystroke.
 */
export function workspaceMetaLine(row: Pick<WorkspaceRowModel, "branch" | "path">): string {
  return [row.branch, row.path].filter(Boolean).join(" · ");
}

/** The verb a row prints beside an agent's name. */
export function activityWord(state: AgentDisplayState): string {
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
