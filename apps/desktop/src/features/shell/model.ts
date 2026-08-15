import type { Pane, Session, TmuxSnapshot, Window as TmuxWindow } from "../../app/types";
import { renderedPanes } from "../terminal/layout";
import type { AppOwnedTab, PersistedAppState, WorkspaceUiRecord } from "./types";
import type { GitDiffTarget, GitStatusEntry, GitStatusSnapshot } from "../git/types";
import type { AgentAttentionRollup } from "../agents/types";

export type CombinedTab =
  | { key: `terminal:${string}`; kind: "terminal"; id: string; title: string; index: number; activeInTmux: boolean; zoomed: boolean; canMoveLeft: boolean; canMoveRight: boolean; attention: AgentAttentionRollup["state"] }
  | { key: `app:${string}`; kind: "app"; id: string; title: string; appKind: AppOwnedTab["kind"]; resource: string; order: number; preview: boolean; canMoveLeft: boolean; canMoveRight: boolean };

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

export function combineWorkspaceTabs(
  windows: readonly TmuxWindow[],
  appTabs: readonly AppOwnedTab[],
  attentionByWindow?: ReadonlyMap<string, AgentAttentionRollup>,
): CombinedTab[] {
  const terminalTabs: CombinedTab[] = [...windows]
    .sort((left, right) => left.index - right.index || left.id.localeCompare(right.id))
    .map((window, index, ordered) => ({
      key: `terminal:${window.id}`,
      kind: "terminal",
      id: window.id,
      title: window.name,
      index: window.index,
      activeInTmux: window.active,
      zoomed: Boolean(window.zoomed),
      canMoveLeft: index > 0,
      canMoveRight: index < ordered.length - 1,
      attention: attentionByWindow?.get(window.id)?.state ?? "none",
    }));
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
    }));
  return [...terminalTabs, ...ownedTabs];
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
  const workspaceUi = state.workspaceUi.filter((item) => {
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
  return changed ? { ...state, appTabs, workspaceUi } : state;
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
  options: { preview: boolean } = { preview: false },
): PersistedAppState {
  const inWorkspace = (tab: AppOwnedTab) => tab.hostProfileId === currentHostProfileId
    && tab.serverIdentity === currentServerIdentity
    && tab.sessionId === session.id;
  const existing = state.appTabs.find((tab) => inWorkspace(tab) && tab.resource === resource);
  if (existing) {
    const pinned = !options.preview && existing.preview
      ? state.appTabs.map((tab) => tab.id === existing.id ? withoutPreview(tab) : tab)
      : state.appTabs;
    return selectAppTab({ ...state, appTabs: pinned }, currentHostProfileId, currentServerIdentity, session, existing.id);
  }

  const details = {
    kind,
    resource,
    title: fileTabTitle(resource),
    rootPath: root.path,
    rootToken: root.token,
    ...(kind === "markdown" ? { viewMode: "split" as const } : {}),
  };
  // The slot is reused, not the record: everything that described the previous
  // file — its markdown view mode, its root snapshot — is replaced, and only
  // the tab's identity and position survive.
  const reusable = options.preview ? state.appTabs.find((tab) => inWorkspace(tab) && tab.preview) : undefined;
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

/** Absent, not `false` — the shape the persistence contract expects. */
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

export function openGitDiffTab(
  state: PersistedAppState,
  currentHostProfileId: string,
  currentServerIdentity: string,
  session: Session,
  entry: GitStatusEntry,
  target: GitDiffTarget,
  status: GitStatusSnapshot,
  root: { path: string; token: string },
): PersistedAppState {
  const existing = state.appTabs.find((tab) => tab.hostProfileId === currentHostProfileId
    && tab.serverIdentity === currentServerIdentity
    && tab.sessionId === session.id
    && tab.kind === "gitDiff"
    && tab.gitRepositoryId === status.repository.id
    && tab.gitPath === entry.path
    && tab.gitTarget === target);
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
  const tab: AppOwnedTab = existing ? { ...existing, ...details } : {
    id: crypto.randomUUID(),
    hostProfileId: currentHostProfileId,
    serverIdentity: currentServerIdentity,
    sessionId: session.id,
    sessionName: session.name,
    kind: "gitDiff",
    order: state.appTabs.filter((candidate) => candidate.hostProfileId === currentHostProfileId
      && candidate.serverIdentity === currentServerIdentity && candidate.sessionId === session.id).length,
    ...details,
  };
  const appTabs = existing ? state.appTabs.map((candidate) => candidate.id === tab.id ? tab : candidate) : [...state.appTabs, tab];
  return selectAppTab({ ...state, appTabs }, currentHostProfileId, currentServerIdentity, session, tab.id);
}

export function setMarkdownViewMode(
  state: PersistedAppState,
  currentHostProfileId: string,
  tabId: string,
  viewMode: "source" | "preview" | "split",
): PersistedAppState {
  return {
    ...state,
    appTabs: state.appTabs.map((tab) => tab.hostProfileId === currentHostProfileId && tab.id === tabId
      ? { ...tab, viewMode }
      : tab),
  };
}

export function closeAppTab(state: PersistedAppState, currentHostProfileId: string, tabId: string): PersistedAppState {
  const appTabs = state.appTabs.filter((tab) => tab.hostProfileId !== currentHostProfileId || tab.id !== tabId);
  const workspaceUi = state.workspaceUi.map((item) => item.hostProfileId === currentHostProfileId && item.selectedAppTabId === tabId
    ? { ...item, selectedAppTabId: undefined }
    : item);
  return { ...state, appTabs, workspaceUi };
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
): Session | undefined {
  const ordered = orderedSessions(sessions);
  return ordered.find((session) => session.id === currentId)
    ?? ordered.find((session) => session.name === previousName)
    ?? ordered[0];
}

export function mountedTerminalPanes(
  panes: readonly Pane[],
  activeWindowId: string | undefined,
  appTabSelected: boolean,
  zoomed: boolean,
): Pane[] {
  if (!activeWindowId || appTabSelected) return [];
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
