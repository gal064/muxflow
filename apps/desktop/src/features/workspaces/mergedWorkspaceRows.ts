import type { TmuxSnapshot } from "../../app/types";
import type { AgentAdapterDescriptor, AgentAttentionRollup, AgentRecord } from "../agents/types";
import type { HostScopeToken } from "../shell/hostScope";
import type { ConnectionPhase } from "../../state/connectionReducer";
import { workspaceRows, type WorkspaceRowModel } from "./workspaceRows";

/** Everything one shown host contributes to the merged workspace list. */
export interface HostRowSource {
  hostProfileId: string;
  letter: string;
  label: string;
  phase: ConnectionPhase;
  canMutate: boolean;
  transport: "local" | "ssh";
  scope: HostScopeToken;
  snapshot: TmuxSnapshot;
  /** Only the active host has one. */
  activeSessionId?: string;
  agents: readonly AgentRecord[];
  adapters: readonly AgentAdapterDescriptor[];
  attentionByWorkspace: ReadonlyMap<string, AgentAttentionRollup>;
  activeBranch?: string;
  home?: string;
}

/**
 * A workspace row that knows which host it came from.
 *
 * Session ids repeat across hosts (`$0` exists on every tmux server), so the
 * row carries its own key and its host's scope: every callback the sidebar
 * fires hands the row over whole, and the receiver acts on the row's host
 * rather than on whichever host happens to be active.
 */
export interface MergedWorkspaceRow extends WorkspaceRowModel {
  key: string;
  hostProfileId: string;
  /** "" when letters are hidden — one shown host has nothing to tell apart. */
  letter: string;
  scope: HostScopeToken;
  /** The host's link phase; a row whose host is not connected is drawn dimmed. */
  phase: ConnectionPhase;
  canMutate: boolean;
}

/** One host's rows in that host's own order, tagged with the host. */
export function hostWorkspaceRows(source: HostRowSource, showLetters: boolean): MergedWorkspaceRow[] {
  return workspaceRows({
    snapshot: source.snapshot,
    activeSessionId: source.activeSessionId,
    agents: source.agents,
    adapters: source.adapters,
    attentionByWorkspace: source.attentionByWorkspace,
    activeBranch: source.activeBranch,
    home: source.home,
  }).map((row) => ({
    ...row,
    key: `${source.hostProfileId}\0${row.session.id}`,
    hostProfileId: source.hostProfileId,
    letter: showLetters ? source.letter : "",
    scope: source.scope,
    phase: source.phase,
    canMutate: source.canMutate,
  }));
}

/**
 * Every shown host's workspaces as one list: all pinned rows first, then the
 * rest, sources in the order given and each host's own order inside.
 *
 * ⌘1–⌘9 and ⌘P read this list, so a pin on any host lifts its row above every
 * unpinned row on every host — the same "pinned block on top" the single-host
 * list had, extended across hosts rather than repeated per host.
 */
export function mergedWorkspaceRows(sources: readonly HostRowSource[], showLetters: boolean): MergedWorkspaceRow[] {
  return mergeHostRows(sources.map((source) => hostWorkspaceRows(source, showLetters)));
}

/**
 * The merge alone, over per-host lists the caller already built — so each
 * host's `hostWorkspaceRows` can be memoized on its own inputs and one
 * host's event never re-sorts another host's rows.
 */
export function mergeHostRows(perHost: readonly (readonly MergedWorkspaceRow[])[]): MergedWorkspaceRow[] {
  return [
    ...perHost.flatMap((rows) => rows.filter((row) => row.pinned)),
    ...perHost.flatMap((rows) => rows.filter((row) => !row.pinned)),
  ];
}

/**
 * The sidebar's pinned-only filter over the merged list.
 *
 * The workspace on screen keeps its row, as it does single-host, but only on
 * the active host: the same session id on a peer host is a different
 * workspace, and it has no claim to the exemption.
 */
export function pinnedOnlyMergedRows(
  rows: readonly MergedWorkspaceRow[],
  active: { hostProfileId: string; sessionId?: string },
): MergedWorkspaceRow[] {
  return rows.filter((row) => row.pinned
    || (row.hostProfileId === active.hostProfileId && row.session.id === active.sessionId));
}
