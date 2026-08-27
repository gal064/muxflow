import type { Pane, Session, TmuxSnapshot, Window as TmuxWindow } from "../../app/types";
import { renderedPanes } from "../terminal/layout";
import { MAX_ARCHIVED_WORKSPACES, usableWorkspaceDefault } from "./types";
import { pinnedFirst, unpinTab, unpinWorkspaceAndTabs } from "./pins";
import type { AppOwnedTab, AppTabViewMode, ArchivedWorkspaceRecord, PersistedAppState, WorkspaceDefaults, WorkspaceUiRecord } from "./types";
import type { GitDiffTarget, GitStatusEntry, GitStatusSnapshot } from "../git/types";
import type { AgentAdapterId, AgentAttentionRollup, AgentTopologyAuthority } from "../agents/types";
import { stripAgentStatusGlyphs } from "../agents/agentLabels";

export type CombinedTab =
  | { key: `terminal:${string}`; kind: "terminal"; id: string; title: string; index: number; activeInTmux: boolean; zoomed: boolean; canMoveLeft: boolean; canMoveRight: boolean; attention: AgentAttentionRollup["state"]; agentAdapterId?: AgentAdapterId; agentPresence: TerminalAgentPresence; pinned: boolean }
  | { key: `app:${string}`; kind: "app"; id: string; title: string; appKind: AppOwnedTab["kind"]; resource: string; order: number; preview: boolean; canMoveLeft: boolean; canMoveRight: boolean; pinned: boolean }
  | { key: `pending:${string}`; kind: "pending"; title: string };
export type SelectableTab = Exclude<CombinedTab, { kind: "pending" }>;
export type TerminalAgentPresence = "present" | "absent" | "unknown";
export interface AgentPresenceSnapshot {
  accepted?: AgentTopologyAuthority;
  current?: Omit<AgentTopologyAuthority, "coveredWindowIds">;
  byWindow: ReadonlyMap<string, AgentAttentionRollup>;
  /** An authoritative live record exists but cannot prove which window owns it. */
  hasUnmappedAgents?: boolean;
}

/** The exact ordered set addressed by Control-1…9 and drawn with numbers. */
export function selectableTabs(tabs: readonly CombinedTab[]): SelectableTab[] {
  return tabs.filter((tab): tab is SelectableTab => tab.kind !== "pending");
}

/** One canonical answer for both menu candidates and mutation-time rechecks. */
export function agentPresenceIsCurrent(presence: AgentPresenceSnapshot, minimumGeneration = 0): boolean {
  const accepted = presence.accepted;
  const current = presence.current;
  return Boolean(accepted && current
    && accepted.hostProfileId === current.hostProfileId
    && accepted.serverIdentity === current.serverIdentity
    && accepted.connectionEpoch === current.connectionEpoch
    && accepted.topologyGeneration === current.topologyGeneration
    && current.topologyGeneration >= minimumGeneration);
}

/** One canonical answer for both menu candidates and mutation-time rechecks. */
export function terminalAgentPresence(windowId: string, presence: AgentPresenceSnapshot): TerminalAgentPresence {
  if (presence.hasUnmappedAgents || !agentPresenceIsCurrent(presence)
    || !presence.accepted?.coveredWindowIds.has(windowId)) return "unknown";
  return (presence.byWindow.get(windowId)?.total ?? 0) > 0 ? "present" : "absent";
}

/**
 * A tab that has been asked for but does not exist on the host yet.
 *
 * Creating a window is a round trip, and until it came back the strip showed
 * nothing at all: the click had no visible effect, which reads as a dropped
 * click rather than as waiting. This is the placeholder that occupies the gap.
 *
 * Deliberately not an optimistic tab. Nothing is reconciled onto it and it
 * never carries a temporary id that later has to become a real one — it is a
 * picture of a request in flight, and it retires when the real window arrives
 * in a snapshot or the request fails.
 */
export interface PendingShellTab {
  /** Distinguishes one create from the next; also the React key. */
  key: string;
  /**
   * Whose strip it belongs in. Unset until a create-session round trip comes
   * back, because before that there is no workspace to draw it in — drawing it
   * in the workspace being navigated *away* from would be a lie.
   */
  sessionId?: string;
  /**
   * The real window, once the ack has named it. The placeholder outlives the
   * ack on purpose: the ack is not the snapshot, and retiring on the ack alone
   * would blink the strip back to empty until the snapshot carrying the new
   * window lands.
   */
  windowId?: string;
  title: string;
}

export interface AgentShellItem {
  id: string;
  label: string;
  state: "working" | "blocked" | "idle" | "unknown" | "done";
  hostProfileId: string;
  serverIdentity: string;
  sessionId: string;
  windowId: string;
  paneId: string;
  updatedAt: number;
}

export function orderedSessions(sessions: readonly Session[]): Session[] {
  return [...sessions].sort((left, right) => (left.order ?? Number.MAX_SAFE_INTEGER) - (right.order ?? Number.MAX_SAFE_INTEGER)
    || left.name.localeCompare(right.name)
    || left.id.localeCompare(right.id));
}

export function appTabsForWorkspace(
  state: PersistedAppState,
  currentHostProfileId: string,
  currentServerIdentity: string | undefined,
  session: Session | undefined,
): AppOwnedTab[] {
  if (!session) return [];
  return state.appTabs
    .filter((tab) => Boolean(currentServerIdentity)
      && tab.hostProfileId === currentHostProfileId
      && tab.serverIdentity === currentServerIdentity
      && tab.sessionId === session.id)
    .sort((left, right) => left.order - right.order || left.id.localeCompare(right.id));
}

/**
 * Whether a placeholder still has a gap to fill.
 *
 * Retires the moment its window exists, so the strip never shows the
 * placeholder and the real tab side by side for the frame between the snapshot
 * arriving and anything else noticing.
 */
function pendingTabStillOpen(pending: PendingShellTab, windows: readonly TmuxWindow[]): boolean {
  return !pending.windowId || !windows.some((window) => window.id === pending.windowId);
}

/**
 * The placeholder's one-way retirement, as a step over each window list.
 *
 * `pendingTabStillOpen` is a *live* predicate: it hides the placeholder while
 * the window exists and shows it again the moment that window is closed. On its
 * own that made an app-created window's placeholder come back — italic, titled
 * "New window", with nothing behind it — the instant the real tab was closed,
 * and nothing could dismiss it: a placeholder has no context menu, the close
 * path returns early for it, and the bulk-close helpers filter it out.
 *
 * Feeding the result back in is what makes it a latch. Once the snapshot names
 * the window, this returns `undefined` and keeps returning it, because
 * `undefined` is the only thing left to step. The anti-blink contract is
 * untouched: the placeholder still outlives the ack and retires only on the
 * snapshot that contains its window.
 */
export function retirePendingTab(
  pending: PendingShellTab | undefined,
  windows: readonly TmuxWindow[],
): PendingShellTab | undefined {
  if (!pending) return undefined;
  return pendingTabStillOpen(pending, windows) ? pending : undefined;
}

export function combineWorkspaceTabs(
  windows: readonly TmuxWindow[],
  appTabs: readonly AppOwnedTab[],
  attentionByWindow?: ReadonlyMap<string, AgentAttentionRollup>,
  pending?: PendingShellTab,
  // `hasUnmappedAgents` belongs here as much as the authority stamps do:
  // dropping it made the strip believe a window was empty while the
  // commit-time recheck, reading the full snapshot, called it unknown.
  authority: Omit<AgentPresenceSnapshot, "byWindow"> = {},
  /** When each of this workspace's tabs was pinned; see `pinnedFirst`. */
  pinnedAt: ReadonlyMap<string, number> = new Map(),
): CombinedTab[] {
  const agentPresence = {
    ...authority,
    byWindow: attentionByWindow ?? new Map<string, AgentAttentionRollup>(),
  };
  const terminalTabs: CombinedTab[] = [...windows]
    .sort((left, right) => left.index - right.index || left.id.localeCompare(right.id))
    .map((window, index, ordered) => {
      const attention = attentionByWindow?.get(window.id);
      const presence = terminalAgentPresence(window.id, agentPresence);
      return {
        key: `terminal:${window.id}`,
        kind: "terminal",
        id: window.id,
        // Keep display normalization independent of agent authority. Animated
        // titles advance tmux's topology generation before the corresponding
        // agent snapshot arrives; keying this strip to `presence` therefore
        // alternated between the raw glyph and the stripped title every frame.
        title: stripAgentStatusGlyphs(window.name),
        index: window.index,
        activeInTmux: window.active,
        zoomed: Boolean(window.zoomed),
        canMoveLeft: index > 0,
        canMoveRight: index < ordered.length - 1,
        attention: attention?.state ?? "none",
        // Identity, not state: which adapter is in this window, so the strip can
        // draw the same mark the sidebar draws for that agent.
        agentAdapterId: attention?.adapterId,
        // Presence is deliberately independent of attention. Idle, unknown and
        // already-read agents are just as protected by Close All Non-Agent Tabs
        // as working, blocked and unread-complete agents.
        agentPresence: presence,
        pinned: pinnedAt.has(window.id),
      };
    });
  const ownedTabs: CombinedTab[] = [...appTabs]
    .sort((left, right) => left.order - right.order || left.id.localeCompare(right.id))
    .map((tab, index, ordered) => ({
      key: `app:${tab.id}`,
      kind: "app",
      id: tab.id,
      title: tab.title,
      appKind: tab.kind,
      resource: tab.resource,
      order: tab.order,
      preview: Boolean(tab.preview),
      canMoveLeft: index > 0,
      canMoveRight: index < ordered.length - 1,
      pinned: pinnedAt.has(tab.id),
    }));
  const pendingTabs: CombinedTab[] = pending && pendingTabStillOpen(pending, windows)
    ? [{ key: `pending:${pending.key}`, kind: "pending", title: pending.title }]
    : [];
  // Pinned tabs lead, in the order they were pinned; everything else keeps the
  // window/document order above. `canMoveLeft` and `canMoveRight` are
  // deliberately *not* recomputed against this: a move is a change to the tmux
  // window index or the document order, which is what those flags describe.
  //
  // The placeholder stays last, because it is the newest thing asked for and
  // because a placeholder that pushed the existing tabs sideways would move the
  // targets under a person's cursor while they waited.
  return [...pinnedFirst([...terminalTabs, ...ownedTabs], (tab) => tab.kind === "pending" ? undefined : pinnedAt.get(tab.id)), ...pendingTabs];
}

/**
 * The two bulk closes, over the strip's own visual order.
 *
 * `tabs` **is** the display order — the array `combineWorkspaceTabs` produced —
 * so "to the right" is a position in it and nothing else. Both refuse an anchor
 * the strip no longer holds: a menu outlives the list it was opened over, and
 * "close everything except a tab that is already gone" is not what the person
 * asked for. That is the same rule `resolveCommandTarget` applies to a stale
 * explicit target — no subject rather than a fallback subject.
 *
 * A placeholder is never a target: there is nothing on the host to close.
 */
export function tabsToCloseOthers(tabs: readonly CombinedTab[], anchorKey: string): CombinedTab[] {
  if (!tabs.some((tab) => tab.key === anchorKey)) return [];
  return tabs.filter((tab) => tab.kind !== "pending" && tab.key !== anchorKey);
}

export function tabsToCloseRight(tabs: readonly CombinedTab[], anchorKey: string): CombinedTab[] {
  const anchor = tabs.findIndex((tab) => tab.key === anchorKey);
  if (anchor < 0) return [];
  return tabs.slice(anchor + 1).filter((tab) => tab.kind !== "pending");
}

/** Every real strip tab that does not contain an agent, in display order. */
export function tabsToCloseNonAgent(tabs: readonly CombinedTab[]): CombinedTab[] {
  return tabs.filter((tab) => tab.kind === "app" || (tab.kind === "terminal" && tab.agentPresence === "absent"));
}

export interface BulkCloseTargets {
  others: CombinedTab[];
  right: CombinedTab[];
  nonAgent: CombinedTab[];
  /** A set holding a tmux window needs the write permission a single close does. */
  takesTerminals(targets: readonly CombinedTab[]): boolean;
  /** Nothing to close, or nothing this connection is allowed to close. */
  disabled(targets: readonly CombinedTab[]): boolean;
}

/**
 * Every bulk close a strip anchor offers, and the one rule that greys them out.
 *
 * The strip now exposes the same two closes twice — as toolbar buttons over the
 * active tab and as menu items over whichever tab was right-clicked. Two call
 * sites computing "what would this close" separately is how a button and a menu
 * come to disagree about the set, so both ask this and neither owns the answer.
 */
export function bulkCloseTargets(
  tabs: readonly CombinedTab[],
  anchorKey: string | undefined,
  canMutate: boolean,
): BulkCloseTargets {
  const takesTerminals = (targets: readonly CombinedTab[]) => targets.some((tab) => tab.kind === "terminal");
  return {
    // No anchor is not a fallback anchor: with nothing selected there is no
    // "others" and no "to the right", and both sets come back empty.
    others: anchorKey === undefined ? [] : tabsToCloseOthers(tabs, anchorKey),
    right: anchorKey === undefined ? [] : tabsToCloseRight(tabs, anchorKey),
    nonAgent: tabsToCloseNonAgent(tabs),
    takesTerminals,
    disabled: (targets) => targets.length === 0 || (takesTerminals(targets) && !canMutate),
  };
}

/**
 * Revalidates the safety predicate at the mutation boundary. A confirmation or
 * editor flush can outlive the snapshot that built the menu, so a terminal is
 * eligible only when the latest authoritative agent snapshot still says it is
 * empty. App-owned tabs never contain agents and remain eligible.
 */
export function tabsEligibleAtBulkCloseCommit(
  tabs: readonly CombinedTab[],
  protectAgents: boolean,
  presence: AgentPresenceSnapshot,
): CombinedTab[] {
  if (!protectAgents) return [...tabs];
  return tabs.filter((tab) => tab.kind === "app"
    || (tab.kind === "terminal" && terminalAgentPresence(tab.id, presence) === "absent"));
}

/**
 * What a bulk close has to say when it did not close everything.
 *
 * A per-tab message would be one notice per survivor, each overwriting the
 * last, and the loop that produced them kept only the final error — so seven
 * tabs asked for, five gone, read as complete success. One sentence, counted,
 * is the whole report; `undefined` means every tab closed and the strip is the
 * message.
 *
 * A tab held back by the commit-time agent recheck is a survivor too, and
 * saying nothing about it was the same silence in a quieter form: the close
 * appeared to succeed while the terminal stayed.
 */
export function bulkCloseOutcomeStatus(closed: number, failed: number, skipped = 0): string | undefined {
  if (failed <= 0 && skipped <= 0) return undefined;
  const attempted = closed + failed + skipped;
  const survivors = [
    failed > 0 ? `${failed} could not be closed` : undefined,
    skipped > 0 ? `${skipped} still had an agent and ${skipped === 1 ? "was" : "were"} left open` : undefined,
  ].filter((clause): clause is string => Boolean(clause));
  return `Closed ${closed} of ${attempted} ${attempted === 1 ? "tab" : "tabs"}; ${survivors.join("; ")}.`;
}

/**
 * What a bulk close says when it closed everything it was given.
 *
 * The confirmation dialog used to be the acknowledgement — you said "Close 4
 * tabs?" and the strip emptying was the answer. With the dialog gone, a bulk
 * close over clipped tabs can remove tabs nobody could see, so the receipt
 * moves after the fact: one counted sentence, auto-dismissing, rather than a
 * question asked before anything happened.
 *
 * `undefined` when nothing was actually closed — an empty receipt is worse
 * than none.
 */
export function bulkCloseCompleteStatus(closed: number): string | undefined {
  return closed > 0 ? `Closed ${closed} ${closed === 1 ? "tab" : "tabs"}.` : undefined;
}

export function workspaceUiRecord(
  state: PersistedAppState,
  currentHostProfileId: string,
  currentServerIdentity: string | undefined,
  session: Session | undefined,
): WorkspaceUiRecord | undefined {
  if (!session) return undefined;
  return state.workspaceUi.find((item) => Boolean(currentServerIdentity)
    && item.hostProfileId === currentHostProfileId
    && item.serverIdentity === currentServerIdentity
    && item.sessionId === session.id);
}

export function reconcileWorkspaceIdentity(
  state: PersistedAppState,
  currentHostProfileId: string,
  currentServerIdentity: string | undefined,
  sessions: readonly Session[],
): PersistedAppState {
  const sessionIds = new Set(sessions.map((session) => session.id));
  let changed = false;
  const appTabs = state.appTabs.filter((tab) => {
    const keep = tab.hostProfileId !== currentHostProfileId || tab.serverIdentity !== currentServerIdentity || sessionIds.has(tab.sessionId);
    if (!keep) changed = true;
    return keep;
  }).map((tab) => {
    if (tab.hostProfileId !== currentHostProfileId) return tab;
    const exact = tab.serverIdentity === currentServerIdentity
      ? sessions.find((session) => session.id === tab.sessionId)
      : undefined;
    if (exact) {
      if (tab.sessionName === exact.name) return tab;
      changed = true;
      return { ...tab, sessionName: exact.name };
    }
    return tab;
  });
  const reconcileRecords = <T extends WorkspaceUiRecord | ArchivedWorkspaceRecord>(records: readonly T[]): T[] => records.filter((item) => {
    const keep = item.hostProfileId !== currentHostProfileId || item.serverIdentity !== currentServerIdentity || sessionIds.has(item.sessionId);
    if (!keep) changed = true;
    return keep;
  }).map((item) => {
    if (item.hostProfileId !== currentHostProfileId) return item;
    const exact = item.serverIdentity === currentServerIdentity
      ? sessions.find((session) => session.id === item.sessionId)
      : undefined;
    if (exact) {
      if (item.sessionName === exact.name) return item;
      changed = true;
      return { ...item, sessionName: exact.name };
    }
    return item;
  });
  const workspaceUi = reconcileRecords(state.workspaceUi);
  // Same rule as `workspaceUi`: a record for a session this server no longer
  // has is a record for a session that was killed from another tmux client,
  // and keeping it would hide whichever session next takes that id.
  const archivedWorkspaces = reconcileRecords(state.archivedWorkspaces);
  return changed ? { ...state, appTabs, workspaceUi, archivedWorkspaces } : state;
}

function archivedRecordMatches(
  record: ArchivedWorkspaceRecord,
  hostProfileId: string,
  serverIdentity: string | undefined,
): boolean {
  return Boolean(serverIdentity) && record.hostProfileId === hostProfileId && record.serverIdentity === serverIdentity;
}

/** The sessions archived on exactly this host and tmux server; none without a server. */
export function archivedSessionIds(
  state: PersistedAppState,
  hostProfileId: string,
  serverIdentity: string | undefined,
): ReadonlySet<string> {
  const ids = new Set<string>();
  for (const record of state.archivedWorkspaces) {
    if (archivedRecordMatches(record, hostProfileId, serverIdentity)) ids.add(record.sessionId);
  }
  return ids;
}

/** Puts a workspace away. No tmux action: the session and its processes keep running. */
export function archiveWorkspace(
  state: PersistedAppState,
  hostProfileId: string,
  serverIdentity: string | undefined,
  session: Session,
  now: number,
): PersistedAppState {
  if (!serverIdentity || archivedSessionIds(state, hostProfileId, serverIdentity).has(session.id)) return state;
  const record: ArchivedWorkspaceRecord = { hostProfileId, serverIdentity, sessionId: session.id, sessionName: session.name, archivedAt: now };
  const archivedWorkspaces = [...state.archivedWorkspaces, record];
  if (archivedWorkspaces.length > MAX_ARCHIVED_WORKSPACES) {
    // The oldest goes. Its session, if still alive, simply reappears in the
    // sidebar, which is the least surprising thing a cap can do.
    let oldest = 0;
    archivedWorkspaces.forEach((item, index) => { if (item.archivedAt < archivedWorkspaces[oldest].archivedAt) oldest = index; });
    archivedWorkspaces.splice(oldest, 1);
  }
  // Archiving takes the pin with it. A pinned row is a row the user asked to
  // keep at the top of the sidebar, and a row that is no longer in the sidebar
  // cannot be at the top of it; keeping the record would silently restore the
  // pin on the next unarchive.
  return unpinWorkspaceAndTabs({ ...state, archivedWorkspaces }, hostProfileId, serverIdentity, session.id);
}

export function unarchiveWorkspace(
  state: PersistedAppState,
  hostProfileId: string,
  serverIdentity: string | undefined,
  sessionId: string,
): PersistedAppState {
  const archivedWorkspaces = state.archivedWorkspaces.filter((record) =>
    !(archivedRecordMatches(record, hostProfileId, serverIdentity) && record.sessionId === sessionId));
  return archivedWorkspaces.length === state.archivedWorkspaces.length ? state : { ...state, archivedWorkspaces };
}

/**
 * The archived workspaces that are alive on this server, in sidebar order —
 * what the Archived view lists. A record whose session is gone is not shown:
 * `reconcileWorkspaceIdentity` drops it on the next snapshot anyway, and a row
 * for a session that cannot be restored would be a row with nothing to do.
 */
export function archivedWorkspacesFor(
  state: PersistedAppState,
  hostProfileId: string,
  serverIdentity: string | undefined,
  sessions: readonly Session[],
): Session[] {
  const archived = archivedSessionIds(state, hostProfileId, serverIdentity);
  return orderedSessions(sessions).filter((session) => archived.has(session.id));
}

export function recoverableAppTabCount(
  state: PersistedAppState,
  hostProfileId: string,
  previousServerIdentity: string,
  sessions: readonly Session[],
): number {
  const counts = new Map<string, number>();
  for (const session of sessions) counts.set(session.name, (counts.get(session.name) ?? 0) + 1);
  return state.appTabs.filter((tab) => tab.hostProfileId === hostProfileId
    && tab.serverIdentity === previousServerIdentity
    && counts.get(tab.sessionName) === 1).length;
}

export function recoverAppTabsFromPreviousServer(
  state: PersistedAppState,
  hostProfileId: string,
  previousServerIdentity: string,
  currentServerIdentity: string,
  sessions: readonly Session[],
): PersistedAppState {
  const uniqueByName = new Map<string, Session>();
  const ambiguous = new Set<string>();
  for (const session of sessions) {
    if (uniqueByName.has(session.name)) ambiguous.add(session.name);
    else uniqueByName.set(session.name, session);
  }
  for (const name of ambiguous) uniqueByName.delete(name);
  const appTabs = state.appTabs.map((tab) => {
    const session = tab.hostProfileId === hostProfileId && tab.serverIdentity === previousServerIdentity
      ? uniqueByName.get(tab.sessionName) : undefined;
    // Root tokens are bound to the previous tmux server and filesystem inode.
    // Recovery may rebind the workspace by name, but it must reacquire a root
    // capability from the newly authoritative active pane before file I/O.
    return session ? { ...tab, serverIdentity: currentServerIdentity, sessionId: session.id, rootPath: undefined, rootToken: undefined } : tab;
  });
  const workspaceUi = state.workspaceUi.map((item) => {
    const session = item.hostProfileId === hostProfileId && item.serverIdentity === previousServerIdentity
      ? uniqueByName.get(item.sessionName) : undefined;
    return session ? { ...item, serverIdentity: currentServerIdentity, sessionId: session.id } : item;
  });
  return { ...state, appTabs, workspaceUi };
}

export function discardServerAppState(state: PersistedAppState, hostProfileId: string, serverIdentity: string): PersistedAppState {
  return {
    ...state,
    appTabs: state.appTabs.filter((tab) => tab.hostProfileId !== hostProfileId || tab.serverIdentity !== serverIdentity),
    workspaceUi: state.workspaceUi.filter((item) => item.hostProfileId !== hostProfileId || item.serverIdentity !== serverIdentity),
    archivedWorkspaces: state.archivedWorkspaces.filter((item) => item.hostProfileId !== hostProfileId || item.serverIdentity !== serverIdentity),
    pinnedWorkspaces: state.pinnedWorkspaces.filter((item) => item.hostProfileId !== hostProfileId || item.serverIdentity !== serverIdentity),
    pinnedTabs: state.pinnedTabs.filter((item) => item.hostProfileId !== hostProfileId || item.serverIdentity !== serverIdentity),
  };
}

export function selectAppTab(
  state: PersistedAppState,
  currentHostProfileId: string,
  currentServerIdentity: string,
  session: Session,
  selectedAppTabId: string | undefined,
): PersistedAppState {
  const record: WorkspaceUiRecord = {
    hostProfileId: currentHostProfileId,
    serverIdentity: currentServerIdentity,
    sessionId: session.id,
    sessionName: session.name,
    ...(selectedAppTabId ? { selectedAppTabId } : {}),
  };
  const index = state.workspaceUi.findIndex((item) => item.hostProfileId === currentHostProfileId
    && item.serverIdentity === currentServerIdentity
    && item.sessionId === session.id);
  const workspaceUi = [...state.workspaceUi];
  if (index >= 0) workspaceUi[index] = record;
  else workspaceUi.push(record);
  return { ...state, workspaceUi };
}

/**
 * The document tabs that stay mounted, most recently selected first.
 *
 * The shell keeps the last few selected file and diff surfaces in the tree so
 * that returning to one is a repaint rather than a rebuild — no second remote
 * read, no second Monaco. This is only the bookkeeping: which ids are entitled
 * to a mounted surface, in selection order.
 *
 * Two rules and nothing else. The selected tab is always retained and always
 * first, so the tab a person is looking at can never be the one evicted; and
 * the list is cut to `limit`, which drops the least recently selected. Ids that
 * no longer name a live tab are the caller's to filter out *before* calling —
 * a closed tab must not occupy a slot, and only the caller knows the tab list.
 *
 * Idempotent by construction: calling it again with the same selection returns
 * the same order, which is what lets it run during render.
 */
export function mountedAppTabIds(
  previous: readonly string[],
  selectedId: string | undefined,
  limit: number,
): string[] {
  const promoted = selectedId
    ? [selectedId, ...previous.filter((id) => id !== selectedId)]
    : [...previous];
  // At least one, whatever the limit says: a limit that could evict the
  // selected tab would unmount the surface being looked at.
  return promoted.slice(0, Math.max(1, limit));
}

function fileTabTitle(resource: string): string {
  return resource.split("/").filter(Boolean).at(-1) ?? resource;
}

/**
 * VS Code's open rule, and the invariant behind it: a workspace has **at most
 * one** preview tab.
 *
 * A single click asks for a preview. If a preview tab is already open it is
 * reused in place — same id, same position in the strip — so browsing a
 * directory leaves one tab behind instead of one per file. A double-click,
 * Enter, an explicit Open, or the first edit asks for a pinned tab, and a tab
 * that is already open is only ever promoted by that, never demoted: reopening
 * a pinned file as a preview must not make it disposable again.
 */
export function openFileTab(
  state: PersistedAppState,
  currentHostProfileId: string,
  currentServerIdentity: string,
  session: Session,
  resource: string,
  kind: "file" | "markdown",
  root: { path: string; token: string; revision: string },
  options: { preview: boolean; refreshRoot?: boolean; viewMode?: AppTabViewMode } = { preview: false },
): PersistedAppState {
  const inWorkspace = (tab: AppOwnedTab) => tab.hostProfileId === currentHostProfileId
    && tab.serverIdentity === currentServerIdentity
    && tab.sessionId === session.id;
  const existing = state.appTabs.find((tab) => inWorkspace(tab) && tab.resource === resource);
  if (existing) {
    let reopened = existing;
    if (!options.preview && reopened.preview) reopened = withoutPreview(reopened);
    if (options.refreshRoot) {
      reopened = { ...reopened, rootPath: root.path, rootToken: root.token };
    }
    const appTabs = reopened === existing
      ? state.appTabs
      : state.appTabs.map((tab) => tab.id === existing.id ? reopened : tab);
    return selectAppTab({ ...state, appTabs }, currentHostProfileId, currentServerIdentity, session, existing.id);
  }

  const details = {
    kind,
    resource,
    title: fileTabTitle(resource),
    rootPath: root.path,
    rootToken: root.token,
    // The configured default, read here and only here: an existing tab is
    // returned above with the mode the user put it in, so changing the setting
    // never reaches a tab that is already open, and changing a tab's mode never
    // reaches the setting.
    ...(kind === "markdown" ? { viewMode: options.viewMode ?? "split" } : {}),
  };
  // The slot is reused, not the record: everything that described the previous
  // file — its markdown view mode, its root snapshot — is replaced, and only
  // the tab's identity and position survive.
  // A preview Git diff is not this slot: a diff's identity is a repository,
  // a path and a target, and rewriting one into a file would lose all three.
  const reusable = options.preview
    ? state.appTabs.find((tab) => inWorkspace(tab) && tab.preview && tab.kind !== "gitDiff")
    : undefined;
  if (reusable) {
    const appTabs = state.appTabs.map((tab) => tab.id === reusable.id
      ? { id: tab.id, hostProfileId: tab.hostProfileId, serverIdentity: tab.serverIdentity, sessionId: tab.sessionId, sessionName: tab.sessionName, order: tab.order, preview: true, ...details }
      : tab);
    return selectAppTab({ ...state, appTabs }, currentHostProfileId, currentServerIdentity, session, reusable.id);
  }

  const tab: AppOwnedTab = {
    id: crypto.randomUUID(),
    hostProfileId: currentHostProfileId,
    serverIdentity: currentServerIdentity,
    sessionId: session.id,
    sessionName: session.name,
    order: state.appTabs.filter(inWorkspace).length,
    ...(options.preview ? { preview: true } : {}),
    ...details,
  };
  return selectAppTab({ ...state, appTabs: [...state.appTabs, tab] }, currentHostProfileId, currentServerIdentity, session, tab.id);
}

/**
 * Promote a preview tab to a permanent one.
 *
 * Returns the *same* state object when there is nothing to do, and that
 * identity is load-bearing: the callers are "the user double-clicked" and
 * "the buffer became dirty", and a fresh object for a no-op would re-render
 * the shell and re-run the persistence effect for a tab that is already
 * pinned.
 */
export function pinAppTab(state: PersistedAppState, currentHostProfileId: string, tabId: string): PersistedAppState {
  const target = state.appTabs.find((tab) => tab.hostProfileId === currentHostProfileId && tab.id === tabId);
  if (!target?.preview) return state;
  return { ...state, appTabs: state.appTabs.map((tab) => tab.id === target.id ? withoutPreview(tab) : tab) };
}

/**
 * Absent, not `false` — the shape the persistence contract expects.
 *
 * Only within a session, though: `AppTabRecord.preview` is a plain
 * `Option<bool>` with no `skip_serializing_if`, so a reload hands this field
 * back as `null` for every pinned tab rather than dropping it. Every reader
 * here coerces (`Boolean(tab.preview)`, truthiness, `!target?.preview`), so the
 * three shapes behave alike — but the declared `boolean | undefined` is not the
 * whole truth about a value that has been through the store.
 */
function withoutPreview(tab: AppOwnedTab): AppOwnedTab {
  const { preview: _preview, ...rest } = tab;
  return rest;
}

export function relocateFileTabs(
  state: PersistedAppState,
  currentHostProfileId: string,
  currentServerIdentity: string,
  rootPath: string,
  source: string,
  destination: string,
): PersistedAppState {
  const absoluteDestination = destination.startsWith("/")
    ? destination
    : `${rootPath.replace(/\/+$/u, "")}/${destination}`;
  const resolvedDestination = `/${absoluteDestination.split("/").filter((component) => component && component !== ".").join("/")}`;
  const sourcePrefix = `${source.replace(/\/+$/u, "")}/`;
  let changed = false;
  const appTabs = state.appTabs.map((tab) => {
    if (tab.hostProfileId !== currentHostProfileId || tab.serverIdentity !== currentServerIdentity
      || (tab.kind !== "file" && tab.kind !== "markdown")
      || (tab.resource !== source && !tab.resource.startsWith(sourcePrefix))) return tab;
    const suffix = tab.resource === source ? "" : tab.resource.slice(sourcePrefix.length);
    const resource = suffix ? `${resolvedDestination.replace(/\/+$/u, "")}/${suffix}` : resolvedDestination;
    changed = true;
    return { ...tab, resource, title: fileTabTitle(resource) };
  });
  return changed ? { ...state, appTabs } : state;
}

/** The one Git diff tab a workspace holds for this repository, path and side. */
export function findGitDiffTab(
  state: PersistedAppState,
  currentHostProfileId: string,
  currentServerIdentity: string,
  sessionId: string,
  repositoryId: string,
  path: string,
  target: GitDiffTarget,
): AppOwnedTab | undefined {
  return state.appTabs.find((tab) => tab.hostProfileId === currentHostProfileId
    && tab.serverIdentity === currentServerIdentity
    && tab.sessionId === sessionId
    && tab.kind === "gitDiff"
    && tab.gitRepositoryId === repositoryId
    && tab.gitPath === path
    && tab.gitTarget === target);
}

/**
 * A single click opens the diff as a preview tab, which the strip draws as one
 * and a double-click promotes; the context menu and the palette open it pinned
 * outright. As with files, a tab is only ever promoted by a pinned open, never
 * demoted by a preview one. Navigating away does not close it: the diff tab
 * stays in the strip until it is closed, and only the surface follows.
 */
export function openGitDiffTab(
  state: PersistedAppState,
  currentHostProfileId: string,
  currentServerIdentity: string,
  session: Session,
  entry: GitStatusEntry,
  target: GitDiffTarget,
  status: GitStatusSnapshot,
  root: { path: string; token: string },
  options: { preview?: boolean } = {},
): PersistedAppState {
  const existing = findGitDiffTab(
    state, currentHostProfileId, currentServerIdentity, session.id, status.repository.id, entry.path, target,
  );
  const resource = `${target}:${entry.displayPath}`;
  const details = {
    resource,
    title: `${entry.displayPath.split("/").at(-1) ?? entry.displayPath} (${target})`,
    rootPath: root.path,
    rootToken: root.token,
    gitRepositoryId: status.repository.id,
    gitPath: entry.path,
    ...(entry.originalPath ? { gitOriginalPath: entry.originalPath } : {}),
    gitTarget: target,
    gitStatusGeneration: status.generation,
    gitSourceGeneration: status.sourceGeneration,
  };
  const tab: AppOwnedTab = existing
    ? options.preview || !existing.preview ? { ...existing, ...details } : withoutPreview({ ...existing, ...details })
    : {
      id: crypto.randomUUID(),
      hostProfileId: currentHostProfileId,
      serverIdentity: currentServerIdentity,
      sessionId: session.id,
      sessionName: session.name,
      kind: "gitDiff",
      order: state.appTabs.filter((candidate) => candidate.hostProfileId === currentHostProfileId
        && candidate.serverIdentity === currentServerIdentity && candidate.sessionId === session.id).length,
      ...(options.preview ? { preview: true } : {}),
      ...details,
    };
  const appTabs = existing ? state.appTabs.map((candidate) => candidate.id === tab.id ? tab : candidate) : [...state.appTabs, tab];
  return selectAppTab({ ...state, appTabs }, currentHostProfileId, currentServerIdentity, session, tab.id);
}

/**
 * What new workspaces on one host start with. Never another host's values: an
 * absent entry is "whatever this app did before the setting existed", which is
 * exactly what a host the user has not configured should get.
 */
export function workspaceDefaultsFor(state: PersistedAppState, hostProfileId: string): WorkspaceDefaults {
  return state.workspaceDefaults[hostProfileId] ?? {};
}

/**
 * Edits one host's entry, keeping the other hosts' untouched.
 *
 * A field set to an empty or whitespace-only string is a field being *cleared*,
 * so it is removed rather than stored — and a host left with nothing to say
 * drops out of the map entirely, so an entry only exists while it means
 * something.
 */
export function setWorkspaceDefaults(
  state: PersistedAppState,
  hostProfileId: string,
  patch: Partial<WorkspaceDefaults>,
): PersistedAppState {
  const merged = { ...workspaceDefaultsFor(state, hostProfileId), ...patch };
  // Bounded here as well as on load: the storage side refuses the *whole* save
  // for one over-long field, so an unbounded paste into Settings would freeze
  // every other thing this file persists.
  const directory = usableWorkspaceDefault(merged.directory);
  const startupCommand = usableWorkspaceDefault(merged.startupCommand);
  const next: WorkspaceDefaults = {
    ...(directory ? { directory } : {}),
    ...(startupCommand ? { startupCommand } : {}),
  };
  const workspaceDefaults = { ...state.workspaceDefaults };
  if (next.directory || next.startupCommand) workspaceDefaults[hostProfileId] = next;
  else delete workspaceDefaults[hostProfileId];
  return { ...state, workspaceDefaults };
}

export function setMarkdownViewMode(
  state: PersistedAppState,
  currentHostProfileId: string,
  tabId: string,
  viewMode: AppTabViewMode,
): PersistedAppState {
  return {
    ...state,
    appTabs: state.appTabs.map((tab) => tab.hostProfileId === currentHostProfileId && tab.id === tabId
      ? { ...tab, viewMode }
      : tab),
  };
}

export function closeAppTab(state: PersistedAppState, currentHostProfileId: string, tabId: string): PersistedAppState {
  const closing = state.appTabs.find((tab) => tab.hostProfileId === currentHostProfileId && tab.id === tabId);
  const appTabs = state.appTabs.filter((tab) => tab.hostProfileId !== currentHostProfileId || tab.id !== tabId);
  const workspaceUi = state.workspaceUi.map((item) => item.hostProfileId === currentHostProfileId && item.selectedAppTabId === tabId
    ? { ...item, selectedAppTabId: undefined }
    : item);
  const closed: PersistedAppState = { ...state, appTabs, workspaceUi };
  // The tab is gone, so its pin has nothing to order. Dropped here rather than
  // left to the next reconcile, so a document tab reopened under a fresh id
  // cannot inherit the position of the one that was closed.
  return closing
    ? unpinTab(closed, currentHostProfileId, closing.serverIdentity, closing.sessionId, tabId)
    : closed;
}

export function reorderAppTab(
  state: PersistedAppState,
  currentHostProfileId: string,
  currentServerIdentity: string | undefined,
  session: Session,
  tabId: string,
  direction: "left" | "right",
): PersistedAppState {
  const tabs = appTabsForWorkspace(state, currentHostProfileId, currentServerIdentity, session);
  const current = tabs.findIndex((tab) => tab.id === tabId);
  const target = current + (direction === "left" ? -1 : 1);
  if (current < 0 || target < 0 || target >= tabs.length) return state;
  const orderById = new Map(tabs.map((tab, index) => [tab.id, index]));
  orderById.set(tabs[current].id, target);
  orderById.set(tabs[target].id, current);
  return {
    ...state,
    appTabs: state.appTabs.map((tab) => tab.hostProfileId === currentHostProfileId && orderById.has(tab.id)
      ? { ...tab, order: orderById.get(tab.id)! }
      : tab),
  };
}

export function resolveAgentShellDestination(
  snapshot: TmuxSnapshot,
  item: AgentShellItem,
  currentHostProfileId: string,
  currentServerIdentity: string | undefined,
): Pane | undefined {
  if (!currentServerIdentity
    || item.hostProfileId !== currentHostProfileId
    || item.serverIdentity !== currentServerIdentity) return undefined;
  const pane = snapshot.panes.find((candidate) => candidate.id === item.paneId);
  if (!pane || pane.sessionId !== item.sessionId || pane.windowId !== item.windowId) return undefined;
  if (!snapshot.sessions.some((session) => session.id === item.sessionId)
    || !snapshot.windows.some((window) => window.id === item.windowId && window.sessionId === item.sessionId)) return undefined;
  return pane;
}

export function resolveSelectedSession(
  sessions: readonly Session[],
  currentId: string | undefined,
  previousName: string | undefined,
  excluded?: ReadonlySet<string>,
): Session | undefined {
  // An archived workspace is not a selection target, even if it was the
  // selection when the app last saved: the snapshot moves off it.
  const ordered = orderedSessions(sessions).filter((session) => !excluded?.has(session.id));
  return ordered.find((session) => session.id === currentId)
    ?? ordered.find((session) => session.name === previousName)
    ?? ordered[0];
}

/**
 * The panes that are worth holding a live renderer for.
 *
 * Only the active window's, and only the ones that window actually draws — a
 * zoomed window draws one. A file or diff tab does *not* subtract from this:
 * its surface is drawn over the terminal layer rather than in place of it, so
 * coming back to a terminal tab is a repaint instead of a teardown, an async
 * drain/serialize, a visibility round trip and a rebuild. Changing window or
 * workspace still unmounts, which is where that cost belongs.
 */
export function mountedTerminalPanes(
  panes: readonly Pane[],
  activeWindowId: string | undefined,
  zoomed: boolean,
): Pane[] {
  if (!activeWindowId) return [];
  return renderedPanes(panes.filter((pane) => pane.windowId === activeWindowId), zoomed);
}

export function shouldSurfaceAuthoritativeTerminal(
  appTabSelected: boolean,
  previousSessionId: string | undefined,
  previousActiveWindowId: string | undefined,
  nextSessionId: string | undefined,
  nextActiveWindowId: string | undefined,
): boolean {
  return appTabSelected
    && Boolean(previousSessionId)
    && previousSessionId === nextSessionId
    && Boolean(previousActiveWindowId)
    && Boolean(nextActiveWindowId)
    && previousActiveWindowId !== nextActiveWindowId;
}

export function shellNavigationMode(canMutate: boolean): "authoritative" | "cached" {
  return canMutate ? "authoritative" : "cached";
}
