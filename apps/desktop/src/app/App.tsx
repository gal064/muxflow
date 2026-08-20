import { invoke } from "@tauri-apps/api/core";
import { lazy, Suspense, useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import type { PendingTextPrompt } from "../commands/TextInputDialog";
import { ConfirmationDialog } from "../commands/ConfirmationDialog";
import type { PendingTmuxConfirmation } from "../commands/destructiveConfirmation";
import {
  commandAvailable,
  commandForKeyboardEvent,
  currentPlatform,
  globalShortcutAllowed,
  type ShortcutOverrides,
} from "../commands/registry";
import { useRowCommands } from "../commands/rowCommands";
import type { TerminalPaneController } from "../features/terminal/TerminalPane";
import { adjacentPane, resizeCellsFromPixels, windowGrid, type PaneDirection } from "../features/terminal/layout";
import { sendBinaryInput, sendInput } from "../features/terminal/api";
import type { TerminalInput } from "../features/terminal/TerminalRenderer";
import { setTerminalScreenReaderMode } from "../features/terminal/accessibilityPreference";
import { TauriTerminalTransferClient } from "../features/terminal/terminalTransferApi";
import { useTerminalTransferRegistry } from "../features/terminal/terminalTransferRegistry";
import { abandonPanePaintSpansForScope } from "../perf/probe";
import type { TmuxAction } from "../features/tmux/actions";
import { TauriAgentClient } from "../features/agents/api";
import { buildAgentRows, jumpTarget, unreadCount, type AgentListRow } from "../features/agents/agentsList";
import { loadAgentSoundPreferences, saveAgentSoundPreferences } from "../features/agents/sound";
import { emitTestNotification, notificationPermissionStatus } from "../features/agents/notifications";
import { TauriFileWorkspaceClient } from "../features/files/api";
import { editorFlushRegistry } from "../features/files/editorFlushRegistry";
import { reconcileDownloadStatus, type ActiveDownloadStatus } from "../features/files/downloadStatus";
import { ignoredPathsFromStatus } from "../features/files/ignoredPaths";
import type { FileEntry } from "../features/files/types";
import { TauriGitWorkspaceClient } from "../features/git/api";
import { GitRepositoryStore } from "../features/git/repositoryStore";
import { DisconnectedStrip } from "../features/shell/DisconnectedStrip";
import { SettingsDialog } from "../features/shell/SettingsDialog";
import { TitleBar } from "../features/shell/TitleBar";
import { emptyFocusHistory, pruneFocusHistory, stepFocus, visitFocus, type FocusHistory } from "../features/shell/focusHistory";
import { resetHostLatency, useHostLatency } from "../features/shell/hostLatency";
import { helperConnectionKey, helperUpgradeReducer, initialHelperUpgradeState } from "../features/shell/helperUpgrade";
import { sameHostConnection, type HostScopeToken } from "../features/shell/hostScope";
import { useShellCommands } from "../features/shell/useShellCommands";
import { effectiveRails } from "../features/shell/responsiveShell";
import { usePersistedAppState } from "../features/shell/usePersistedAppState";
import {
  clampedAgentsRatio, panelWidthForWindow, sidebarWidthForWindow,
  PANEL_MIN_WIDTH, SIDEBAR_MIN_WIDTH, type AppOwnedTab, type HostSetupDecision, type ShellState,
} from "../features/shell/types";
import {
  combineWorkspaceTabs,
  closeAppTab,
  mountedAppTabIds,
  mountedTerminalPanes,
  openFileTab,
  openGitDiffTab,
  pinAppTab,
  selectAppTab,
  setMarkdownViewMode,
  shouldSurfaceAuthoritativeTerminal,
  tabsToCloseOthers,
  tabsToCloseRight,
  type CombinedTab,
  type PendingShellTab,
} from "../features/shell/model";
import { useContextMenusOpen } from "../ui/ContextMenu";
import { TabStrip, workspaceTabDomId, workspaceTabPanelDomId } from "../features/workspaces/TabStrip";
import { WorkspaceSidebar } from "../features/workspaces/WorkspaceSidebar";
import { WorkspaceSwitcher } from "../features/workspaces/WorkspaceSwitcher";
import { inferHome, workspaceRows } from "../features/workspaces/workspaceRows";
import type { ConnectionSpec, HostProfile, Pane } from "./types";
import { resolveTerminalDestination } from "./paneRouting";
import { useAppConnectionController } from "./useAppConnectionController";
import { useAppRecoveryController } from "./useAppRecoveryController";
import { useClientResize } from "./useClientResize";
import { useVisibleTerminalSession } from "./useVisibleTerminalSession";
import { commitScopedAppTabClose, reportAnnouncedPaneResult, useShellNavigation } from "./useShellNavigation";
import { useTmuxActionPerformer } from "./useTmuxActionPerformer";
import { windowCellSize } from "../features/terminal/clientSize";
import { useWorkspaceDomainController } from "./useWorkspaceDomainController";
import { AppDialogLayer } from "./AppDialogLayer";
import { TerminalWorkspaceSurface } from "./TerminalWorkspaceSurface";
import { useAppAgentController } from "./useAppAgentController";
import { useAppShellChrome } from "./useAppShellChrome";
import { useAppHostSettingsActions } from "./useAppHostSettingsActions";
import { useMissingHelperRecovery } from "./useMissingHelperRecovery";
import { useAppFileActions } from "./useAppFileActions";
import { AppNoticeLayer } from "./AppNoticeLayer";
import { AppRightPanel } from "./AppRightPanel";

const AppTabSurface = lazy(() => import("../features/shell/AppTabSurface").then((module) => ({ default: module.AppTabSurface })));
const GitDiffSurface = lazy(() => import("../features/git/GitDiffSurface").then((module) => ({ default: module.GitDiffSurface })));

/**
 * How many document tabs keep a mounted surface at once.
 *
 * Switching between two open file tabs used to unmount one surface and mount
 * the other, which re-read the file from the host and rebuilt Monaco for a tab
 * that had been on screen a moment ago — one framed blank frame per switch.
 * The recently used ones stay in the tree instead, hidden the way the terminal
 * layer is, so the switch is a visibility flip.
 *
 * Bounded because each retained tab is a live read, a directory watch lease, an
 * autosave controller and a Monaco instance. Four is the working set a person
 * moves between; the fifth costs a rebuild, exactly as every switch did before.
 */
const MOUNTED_APP_TAB_LIMIT = 4;

/**
 * What makes a mounted document surface a *different* surface.
 *
 * The same key material the single rendered surface carried, moved out onto the
 * layer that wraps it: identity plus the thing it is showing, so a tab that is
 * repointed at another file or another diff rebuilds, and nothing else does.
 */
function appTabLayerKey(tab: AppOwnedTab): string {
  const identity = `${tab.hostProfileId}\0${tab.serverIdentity}\0${tab.sessionId}\0${tab.id}`;
  return tab.kind === "gitDiff"
    ? `${identity}\0${tab.gitRepositoryId}\0${tab.gitPath}\0${tab.gitTarget}`
    : `${identity}\0${tab.resource}`;
}

/**
 * The tab's frame, drawn while its chunk is still being fetched.
 *
 * The gap before a lazy surface resolves used to be a centred "Loading…" line,
 * so opening a file walked through a centred line, then a centred card, then
 * the editor frame: three layouts for one tab. This is the same silhouette the
 * surface itself settles into — an opaque section with the toolbar bar across
 * the top — so the chunk arriving fills the frame instead of replacing it.
 */
function AppTabFrame({ tab }: { tab: AppOwnedTab }) {
  return tab.kind === "gitDiff"
    ? <section aria-label={tab.title} className="git-diff-surface" role="tabpanel">
      <header className="editor-toolbar git-diff-toolbar" />
    </section>
    : <section aria-label={tab.title} className="file-tab-surface" role="tabpanel">
      <header className="editor-toolbar" />
    </section>;
}

/**
 * What a bulk close is actually about to destroy.
 *
 * Only the terminal windows are named: closing a document tab throws nothing
 * away, and a dialog that counted those too would ask for consent to something
 * that needs none.
 */
function bulkCloseDetail(tabs: readonly CombinedTab[]): string {
  const terminals = tabs.filter((tab) => tab.kind === "terminal").length;
  return terminals === 1
    ? "1 terminal window will be closed and its running processes terminated."
    : `${terminals} terminal windows will be closed and their running processes terminated.`;
}

export function App() {
  const [status, setStatus] = useState("Discovering local tmux…");
  const {
    compactViewport, completedDownload, notice, setCompletedDownload, setNotice, windowWidth,
  } = useAppShellChrome(status);
  const [hostSessionSelection, setHostSessionSelection] = useState<{
    clientId: string;
    sessionId: string;
    terminalEpoch: number;
    version: number;
  }>();
  const [activeDownloadStatus, setActiveDownloadStatus] = useState<ActiveDownloadStatus>();
  const agentClient = useMemo(() => new TauriAgentClient(), []);
  const fileClient = useMemo(() => new TauriFileWorkspaceClient(), []);
  const gitClient = useMemo(() => new TauriGitWorkspaceClient(), []);
  // One shared observation per repository, for the sidebar and every diff tab.
  const gitRepositories = useMemo(() => new GitRepositoryStore(gitClient), [gitClient]);
  // The connection controller reports a handshake failure; what to do about one
  // is decided further down this component, with the helper reducer in hand.
  // The indirection is what lets the two be defined in that order.
  const onHandshakeFailure = useRef<(connection: ConnectionSpec) => void>(() => undefined);
  const connectionController = useAppConnectionController({
    agentClient,
    fileClient,
    gitClient,
    onHandshakeFailure: (failed) => onHandshakeFailure.current(failed),
    setStatus,
  });
  const {
    activeSessionId, activeWindowId, appFocused, clientHostProfileId, clientId, clientIdRef, connection,
    connectionDetail, connectionEpoch, connectionMode, currentHostProfileId,
    currentHostScope, dispatchHost, echoLagProbe, hostScopeRef, hostState, hub, optimisticWindow, profileRecovery,
    profiles, selectedProfileId, setActiveSessionId, setActiveWindowId,
    setConnection, setConnectionDetail, setConnectionEpoch, setConnectionMode,
    setProfileRecovery, setProfiles, setSelectedProfileId, setSshConfigPath, setSshTarget,
    snapshot, snapshotRef, sshConfigPath, sshTarget, terminalEpoch, windows,
  } = connectionController;
  const { appState, appStateRecovery, resetAppState, setAppState } = usePersistedAppState(setStatus);
  const [helperState, dispatchHelper] = useReducer(helperUpgradeReducer, initialHelperUpgradeState);
  const [profileResetConfirmation, setProfileResetConfirmation] = useState(false);
  const [hostDeleteConfirmation, setHostDeleteConfirmation] = useState<HostProfile>();
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [workspaceSwitcherOpen, setWorkspaceSwitcherOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  // Deliberately *not* gated on Settings being open. Every global shortcut is
  // suppressed while a modal is up (`modalOpen` below) and the palette cannot
  // be open at the same time as Settings, so a command available only there is
  // a command the palette renders permanently greyed and a bound shortcut that
  // can never fire. The subject stays legible without the picker on screen
  // because nothing happens until the confirmation, which names the host.
  //
  // The store refuses to empty the list, so the last saved host is not offered
  // here either — a disabled control beats a refusal after a confirmation.
  const deletableProfile = profiles.length > 1
    ? profiles.find((profile) => profile.id === selectedProfileId)
    : undefined;
  const [shortcutEditorOpen, setShortcutEditorOpen] = useState(false);
  const [confirmation, setConfirmation] = useState<PendingTmuxConfirmation>();
  // A bulk close waiting on its one summary dialog. Closing tabs the user is
  // *not* looking at is not the single close's "the surface's disappearance is
  // the confirmation" case, so it asks — once, for the whole set.
  const [pendingBulkClose, setPendingBulkClose] = useState<{ tabs: CombinedTab[]; scope: HostScopeToken }>();
  const [textPrompt, setTextPrompt] = useState<PendingTextPrompt>();
  const [appStateResetConfirmation, setAppStateResetConfirmation] = useState(false);
  const [agentSounds, setAgentSounds] = useState(loadAgentSoundPreferences);
  const [agentModalOpen, setAgentModalOpen] = useState(false);
  const [focusHistory, setFocusHistory] = useState<FocusHistory>(emptyFocusHistory);
  const focusHistoryRef = useRef(focusHistory);
  focusHistoryRef.current = focusHistory;
  // Set while navigation itself is moving the app, so the effect that records
  // where the app ended up does not record the intermediate state as a *new*
  // destination — which truncated the forward branch on every back-step across
  // workspaces.
  const historyStep = useRef(false);
  const controllers = useRef(new Map<string, TerminalPaneController>());
  const platform = useMemo(() => currentPlatform(), []);
  const shortcuts = appState.commands.shortcutOverrides as ShortcutOverrides;
  const currentHelperConnectionKey = helperConnectionKey(connection);
  const terminalTransferClient = useMemo(() => new TauriTerminalTransferClient(), []);
  const terminalTransferRegistry = useTerminalTransferRegistry();
  const latency = useHostLatency();
  useEffect(() => dispatchHelper({ type: "reset" }), [currentHelperConnectionKey]);
  onHandshakeFailure.current = useMissingHelperRecovery({
    connection, connectionEpoch, dispatchHelper, setConnectionDetail,
  });
  /**
   * The host whose agent status the *helper install* dialog already consented
   * to, waiting for the connection that install produces.
   *
   * In memory only, and spent by the first prompt it prevents. A relaunch
   * before the host reconnects goes back to the ordinary one-time question,
   * which is the correct fallback: it explains itself and can be declined.
   */
  const [pendingAgentAutoSetup, setPendingAgentAutoSetup] = useState<string>();
  // Renderers read this once, when they are created; changing it must not tear
  // down live terminals, so the setting says panes pick it up as they appear.
  useEffect(() => setTerminalScreenReaderMode(appState.shell.terminalScreenReader), [appState.shell.terminalScreenReader]);

  // A new bridge is a new link; the last one's measured round-trip describes
  // nothing about it.
  useEffect(() => { resetHostLatency(); }, [clientId]);
  // The file client outlives any one bridge, so a connection that has gone must
  // take its shared directory watches with it: the host lost those
  // registrations along with the connection, and the records left behind hold
  // promises nothing can settle.
  useEffect(() => () => { if (clientId) fileClient.retireConnection(clientId); }, [clientId, fileClient]);
  const appRecovery = useAppRecoveryController({
    appState,
    currentHostProfileId,
    currentScope: currentHostScope,
    serverIdentity: hostState.serverIdentity,
    sessions: snapshot.sessions,
    setAppState,
  });

  const {
    activePane, activeSession, activeWindow, fileScope, panes, selectedAppTab,
    terminalTransferScope, workspaceAppTabs, workspaceFiles, workspaceGit,
  } = useWorkspaceDomainController({
    activeSessionId, activeWindowId, appState, clientId, connected: hostState.phase === "connected", connection,
    currentHostProfileId, fileClient, generation: hostState.generation, gitRepositories,
    serverIdentity: hostState.serverIdentity, snapshot, terminalEpoch, windows,
  });
  const selectedAppTabRef = useRef(selectedAppTab);
  selectedAppTabRef.current = selectedAppTab;
  /**
   * Which document tabs keep a mounted surface, and for which workspace.
   *
   * Scoped to the workspace whose strip is on screen — host profile, server and
   * session — and reset when that changes, because `workspaceAppTabs` is
   * already scoped the same way: an id from the workspace being left could
   * never be mounted again, and leaving it in the list would only spend a slot
   * belonging to the workspace being entered.
   *
   * Updated during render rather than in an effect. `mountedAppTabIds` is
   * idempotent, and a tab that has just been selected must be mounted in the
   * same commit that selects it — an effect would mount it one render late,
   * which is the blank frame this removes.
   */
  const retainedAppTabs = useRef<{ workspace: string; ids: string[] }>({ workspace: "", ids: [] });
  const appTabWorkspace = activeSession && hostState.serverIdentity
    ? `${currentHostProfileId}\0${hostState.serverIdentity}\0${activeSession.id}`
    : "";
  if (retainedAppTabs.current.workspace !== appTabWorkspace) {
    retainedAppTabs.current = { workspace: appTabWorkspace, ids: [] };
  }
  const liveAppTabIds = new Set(workspaceAppTabs.map((tab) => tab.id));
  retainedAppTabs.current.ids = mountedAppTabIds(
    // Closed and evicted tabs drop out here rather than inside the rule: a tab
    // the strip no longer has must not hold a slot open for itself.
    retainedAppTabs.current.ids.filter((id) => liveAppTabIds.has(id)),
    selectedAppTab?.id,
    MOUNTED_APP_TAB_LIMIT,
  );
  const retainedAppTabIds = retainedAppTabs.current.ids;
  // In tab-strip order, never selection order: React reconciles the layers by
  // key, and a list that reordered on every selection would move the DOM of
  // surfaces that did not change.
  const mountedAppTabs = workspaceAppTabs.filter((tab) => retainedAppTabIds.includes(tab.id));
  /** Where the files controller and the git controller meet; the rule itself is `ignoredPathsFromStatus`. */
  const ignoredPaths = useMemo(() => ignoredPathsFromStatus(workspaceGit.status), [workspaceGit.status]);

  useEffect(() => {
    if (!activeDownloadStatus) return;
    const reconciled = reconcileDownloadStatus(status, activeDownloadStatus, workspaceFiles.transfers);
    if (reconciled.status !== status) setStatus(reconciled.status);
    if (reconciled.active !== activeDownloadStatus) setActiveDownloadStatus(reconciled.active);
    if (reconciled.completion) setCompletedDownload(reconciled.completion);
  }, [activeDownloadStatus, status, workspaceFiles.transfers]);
  const performAction = useTmuxActionPerformer({
    canMutate: hostState.canMutate,
    clientId,
    generation: hostState.generation,
    hostScopeRef,
    serverIdentity: hostState.serverIdentity,
    setStatus,
  });
  useEffect(() => () => abandonPanePaintSpansForScope(clientId), [clientId]);

  const acknowledgeHostSessionSelection = useCallback((sessionId: string) => {
    const selectedClientId = clientIdRef.current;
    if (!selectedClientId || terminalEpoch === undefined) return;
    setHostSessionSelection((previous) => ({
      clientId: selectedClientId,
      sessionId,
      terminalEpoch,
      version: (previous?.version ?? 0) + 1,
    }));
  }, [clientIdRef, terminalEpoch]);

  const setNavigationAppTab = useCallback((sessionId: string, appTabId: string | undefined) => {
    const scope = hostScopeRef.current;
    const session = snapshotRef.current.sessions.find((item) => item.id === sessionId);
    if (!session || !scope.serverIdentity) return;
    setAppState((current) => selectAppTab(current, scope.hostProfileId, scope.serverIdentity!, session, appTabId));
  }, [setAppState]);
  // The placeholder for a create that has not come back. Held here rather than
  // in the navigation hook because the strip is what draws it, and the hook
  // does not own any rendered state.
  const [pendingTab, setPendingTab] = useState<PendingShellTab | undefined>(undefined);
  const shellNavigation = useShellNavigation({
    activeSessionId,
    activeWindowId,
    acknowledgeHostSessionSelection,
    canMutate: hostState.canMutate,
    currentScope: currentHostScope,
    focusPaneController: (paneId) => controllers.current.get(paneId)?.focus(),
    performAction,
    sessions: snapshot.sessions,
    setActiveSessionId,
    setActiveWindowId,
    setAppTab: setNavigationAppTab,
    optimisticWindow,
    setPendingTab,
    setStatus,
    windows: snapshot.windows,
  });
  const surfacePaneDestination = useCallback((pane: Pane, source: string, successMessage?: string) =>
    shellNavigation.selectPane(pane, { kind: "announce", source, successMessage }), [shellNavigation]);

  const {
    confirmHelperInstall, connect, deleteSavedProfile: deleteSelectedProfile, probeHelper, selectProfile,
    switchHostProfile: switchAgentHostProfile,
  } = useAppHostSettingsActions({
    clearActiveSelection: () => {
      setActiveSessionId(undefined);
      setActiveWindowId(undefined);
    },
    connection,
    connectionMode,
    currentScope: currentHostScope,
    dispatchHelper,
    helperState,
    profiles,
    resetHost: () => dispatchHost({ type: "reset" }),
    scopeIsCurrent: (scope) => sameHostConnection(scope, hostScopeRef.current),
    selectedProfileId,
    setConnection,
    setConnectionDetail,
    setConnectionEpoch,
    setConnectionMode,
    setProfiles,
    setSelectedProfileId,
    setSshConfigPath,
    setSshTarget,
    setStatus,
    sshConfigPath,
    sshTarget,
  });

  const recordHostSetupDecision = useCallback((hostProfileId: string, decision: HostSetupDecision) => {
    setAppState((current) => ({
      ...current,
      hostSetup: { ...current.hostSetup, [hostProfileId]: decision },
    }));
  }, [setAppState]);
  const hostLabel = connection.mode === "local" ? "local" : connection.target;
  const {
    hostSetup: agentHostSetup,
    notificationActivation,
    runtime: agentRuntime,
    scope: agentScope,
    workflow: agentWorkflow,
  } = useAppAgentController({
    activePane,
    activeRoot: workspaceFiles.root,
    activeSession,
    activeSessionId,
    activeWindow,
    activeWindowId,
    agentClient,
    // Only for the host it was given for. The install reconnects, and by the
    // time that connection is up the user may have switched somewhere else —
    // where this consent means nothing and the ordinary prompt is right.
    agentAutoSetup: pendingAgentAutoSetup === currentHostProfileId
      ? { hostProfileId: currentHostProfileId, consume: () => setPendingAgentAutoSetup(undefined) }
      : undefined,
    appFocused,
    clientHostProfileId,
    clientId,
    currentHostProfileId,
    decision: appState.hostSetup[currentHostProfileId],
    decisionsArePersistable: appStateRecovery === undefined,
    hostCanMutate: hostState.canMutate,
    hostLabel,
    profiles,
    recordDecision: recordHostSetupDecision,
    requestReconnect: () => setConnectionEpoch((value) => value + 1),
    selectedAppTab: Boolean(selectedAppTab),
    serverIdentity: hostState.serverIdentity,
    setAgentModalOpen,
    setStatus,
    snapshot,
    soundPreferences: agentSounds,
    surfacePaneDestination,
    switchHostProfile: switchAgentHostProfile,
    terminalEpoch,
    topologyGeneration: hostState.generation,
  });

  const home = useMemo(() => inferHome(snapshot.panes.map((pane) => pane.currentPath)), [snapshot.panes]);
  const sidebarRows = useMemo(() => workspaceRows({
    snapshot,
    activeSessionId,
    agents: agentRuntime.agents,
    attentionByWorkspace: agentRuntime.rollups.byWorkspace,
    activeBranch: workspaceGit.status?.repository.headName,
    home,
  }), [activeSessionId, agentRuntime.agents, agentRuntime.rollups.byWorkspace, home, snapshot, workspaceGit.status]);
  const agentRows = useMemo(() => {
    const orderBySession = new Map(sidebarRows.map((row, index) => [row.session.id, index]));
    const windowIndexById = new Map(snapshot.windows.map((item) => [item.id, item.index]));
    const paneIds = new Set(snapshot.panes.map((pane) => pane.id));
    return buildAgentRows(
      agentRuntime.agents,
      (record) => ({
        workspaceOrder: orderBySession.get(record.sessionId) ?? Number.MAX_SAFE_INTEGER,
        workspaceName: record.sessionName || "unknown workspace",
        tabIndex: windowIndexById.get(record.windowId),
      }),
      (record) => Boolean(record.paneId) && paneIds.has(record.paneId),
      appState.shell.agentSort,
    );
  }, [agentRuntime.agents, appState.shell.agentSort, sidebarRows, snapshot.panes, snapshot.windows]);
  const unread = useMemo(() => unreadCount(agentRows), [agentRows]);

  // Only in its own workspace's strip: a create-session placeholder has no
  // session until its ack names one, and drawing it anywhere before that would
  // put it in the workspace being navigated away from.
  const pendingTabHere = pendingTab && pendingTab.sessionId === activeSessionId ? pendingTab : undefined;
  const combinedTabs = useMemo(
    () => combineWorkspaceTabs(windows, workspaceAppTabs, agentRuntime.rollups.byWindow, pendingTabHere),
    [agentRuntime.rollups.byWindow, pendingTabHere, windows, workspaceAppTabs],
  );
  const activeCombinedTabKey = selectedAppTab ? `app:${selectedAppTab.id}` : activeWindow ? `terminal:${activeWindow.id}` : undefined;
  const grid = useMemo(() => windowGrid(panes), [panes]);
  const mountedPanes = useMemo(
    () => mountedTerminalPanes(snapshot.panes, activeWindowId, Boolean(activeWindow?.zoomed)),
    [activeWindow?.zoomed, activeWindowId, snapshot.panes],
  );
  const lastAuthoritativeWindow = useRef(new Map<string, string>());

  useEffect(() => {
    const authoritative = windows.find((window) => window.active)?.id;
    const identityKey = activeSession && hostState.serverIdentity
      ? `${currentHostProfileId}\0${hostState.serverIdentity}\0${activeSession.id}`
      : undefined;
    const previous = identityKey ? lastAuthoritativeWindow.current.get(identityKey) : undefined;
    const preserveAppTab = activeSession
      ? shellNavigation.observeAuthoritativeWindow(activeSession.id, authoritative, hostState.generation)
      : false;
    if (!preserveAppTab
      && shouldSurfaceAuthoritativeTerminal(Boolean(selectedAppTab), activeSession?.id, previous, activeSession?.id, authoritative)
      && activeSession) {
      setAppState((current) => selectAppTab(current, currentHostProfileId, hostState.serverIdentity!, activeSession, undefined));
    }
    if (identityKey && authoritative) lastAuthoritativeWindow.current.set(identityKey, authoritative);
  }, [activeSession, currentHostProfileId, hostState.generation, hostState.serverIdentity,
    selectedAppTab, shellNavigation.observeAuthoritativeWindow, windows]);

  // Focus history follows where the app actually ended up, whatever moved it —
  // a click, a shortcut, an agent notification, or tmux itself. Except when
  // ⌘[ / ⌘] moved it: that is a walk through the history, not a new
  // destination, and recording it would truncate the branch being walked.
  useEffect(() => {
    if (!activeSessionId) return;
    if (historyStep.current) { historyStep.current = false; return; }
    setFocusHistory((current) => visitFocus(current, { sessionId: activeSessionId, windowId: activeWindowId }));
  }, [activeSessionId, activeWindowId]);
  useEffect(() => {
    setFocusHistory((current) => pruneFocusHistory(current, (point) =>
      snapshot.sessions.some((session) => session.id === point.sessionId)
      && (!point.windowId || snapshot.windows.some((item) => item.id === point.windowId))));
  }, [snapshot.sessions, snapshot.windows]);

  const focusDirection = useCallback((direction: PaneDirection) => {
    if (!activePane) return;
    const target = adjacentPane(panes, activePane, direction);
    if (target) void shellNavigation.selectPane(target, { kind: "silent" });
  }, [activePane, panes, shellNavigation]);

  const selectSession = useCallback((sessionId: string) => {
    notificationActivation.clearNotificationFocusGuard();
    shellNavigation.selectSession(sessionId);
  }, [notificationActivation, shellNavigation]);
  const selectWindow = useCallback((windowId: string) => {
    notificationActivation.clearNotificationFocusGuard();
    shellNavigation.selectWindow(windowId);
  }, [notificationActivation, shellNavigation]);

  const selectCombinedTab = useCallback((tab: CombinedTab) => {
    // A placeholder stands for a window that does not exist yet: there is
    // nothing to select, and inventing a selection here is precisely the
    // temp-id reconciliation this placeholder exists to avoid.
    if (tab.kind === "pending") return;
    if (tab.kind === "terminal") selectWindow(tab.id);
    // No status: the tab the user asked for is now the tab on screen. The
    // message that used to be written here reached nobody either way — the
    // notice is the channel's only reader and it has classified "Opened …" as
    // routine since it was written.
    else if (activeSession && hostState.serverIdentity) {
      shellNavigation.selectAppTab(activeSession.id, activeWindowId, tab.id);
    }
  }, [activeSession, activeWindowId, hostState.serverIdentity, selectWindow, shellNavigation]);

  const selectAgentRow = useCallback((row: AgentListRow, scope = hostScopeRef.current) => {
    notificationActivation.clearNotificationFocusGuard();
    if (!sameHostConnection(scope, hostScopeRef.current)) return;
    if (!row.agent.paneId) return setStatus(`Agent ${row.agent.displayName} has no exact pane match; navigation is unavailable.`);
    const destination = resolveTerminalDestination(snapshot.panes, row.agent.paneId);
    if (destination.kind === "unavailable") return setStatus(`Agent destination ${row.agent.displayName} is no longer available: ${destination.reason}.`);
    void surfacePaneDestination(destination.pane, `Agent ${row.agent.displayName}`)
      .then((result) => reportAnnouncedPaneResult(result, setStatus));
  }, [notificationActivation, snapshot.panes, surfacePaneDestination]);

  // What the Explorer, Git and the agents list currently offer for the row the
  // user last pointed at — the palette's only way to name a row.
  const rowCommands = useRowCommands();
  /**
   * The one commit path for closing an app tab, shared by the single close and
   * by the bulk closes. Neither reaches it through `runCommand`: that route
   * flushes every open editor first, which for a set of tabs would replay the
   * same flush once per tab.
   */
  const closeWorkspaceAppTab = (tab: AppOwnedTab, scope: HostScopeToken) => {
    const commit = () => setAppState((current) => closeAppTab(current, currentHostProfileId, tab.id));
    commitScopedAppTabClose({
      activeWindowId,
      commit,
      currentScope: hostScopeRef.current,
      revealTerminal: shellNavigation.revealLocalTerminal,
      scope,
      selectedAppTabId: selectedAppTabRef.current?.id,
      tabId: tab.id,
      tabSessionId: tab.sessionId,
    });
  };
  const { commandContext, runCommand } = useShellCommands({
    activePane, activeSession, activeWindow, appState, canMutate: hostState.canMutate,

    closeAppTab: closeWorkspaceAppTab,
    combinedTabs, controllers, currentHostProfileId, deletableHostProfile: deletableProfile,
    focusDirection, generation: hostState.generation, hostScope: currentHostScope,
    isHostScopeCurrent: (scope) => sameHostConnection(scope, hostScopeRef.current),
    jumpToUnreadAgent: () => {
      const target = jumpTarget(agentRows);
      if (!target) return setStatus("No agent is waiting on you.");
      selectAgentRow(target);
    },
    performAction, requestHostProfileDelete: setHostDeleteConfirmation, rowCommands, selectedAppTab,
    createSession: shellNavigation.createSession,
    createWindow: (sessionId) => {
      notificationActivation.clearNotificationFocusGuard();
      shellNavigation.createWindow(sessionId);
    },
    selectRelativeTab: (direction) => {
      const index = combinedTabs.findIndex((tab) => tab.key === activeCombinedTabKey);
      const next = combinedTabs[(index < 0 ? 0 : index + direction + combinedTabs.length) % Math.max(1, combinedTabs.length)];
      if (next) selectCombinedTab(next);
    },
    selectTabByIndex: (index) => {
      const tab = combinedTabs[index];
      if (tab) selectCombinedTab(tab);
    },
    selectWorkspaceByIndex: (index) => {
      const row = sidebarRows[index];
      if (row) selectSession(row.session.id);
    },
    serverIdentity: hostState.serverIdentity, setAppState, setConfirmation,
    setPaletteOpen, setSettingsOpen, setShortcutEditorOpen, setStatus, setTextPrompt,
    setWorkspaceSwitcherOpen, snapshot,
    // Navigation happens here, not inside a state updater. React invokes
    // updaters twice under StrictMode, and an updater that dispatched tmux
    // actions therefore sent each one twice, with one captured generation
    // between them — the same double-dispatch the tab strip documents avoiding.
    stepFocusHistory: (direction) => {
      const stepped = stepFocus(focusHistoryRef.current, direction);
      if (!stepped.point) {
        setStatus(direction === "back" ? "Nothing earlier to go back to." : "Nothing later to go forward to.");
        return;
      }
      const { sessionId, windowId } = stepped.point;
      historyStep.current = true;
      setFocusHistory(stepped.history);
      if (sessionId !== activeSessionId) selectSession(sessionId);
      else if (windowId && windowId !== activeWindowId) selectWindow(windowId);
      else historyStep.current = false;
    },
    windows,
  });
  // A context menu is an overlay like any other: with it open, ⌘W must not
  // close the tab behind it.
  const contextMenuOpen = useContextMenusOpen();
  const modalOpen = contextMenuOpen || paletteOpen || workspaceSwitcherOpen || settingsOpen || shortcutEditorOpen
    || Boolean(confirmation) || Boolean(pendingBulkClose) || Boolean(textPrompt)
    || agentModalOpen || agentHostSetup.open || appStateResetConfirmation || appRecovery.modalOpen
    || profileResetConfirmation || Boolean(hostDeleteConfirmation) || helperState.phase === "confirming";

  useEffect(() => {
    const handleKey = (event: KeyboardEvent) => {
      if (!globalShortcutAllowed(event, modalOpen)) return;
      const command = commandForKeyboardEvent(event, platform, shortcuts);
      if (!command || !commandAvailable(command, commandContext)) return;
      event.preventDefault();
      event.stopPropagation();
      void runCommand(command.id);
    };
    window.addEventListener("keydown", handleKey, true);
    return () => window.removeEventListener("keydown", handleKey, true);
  }, [commandContext, modalOpen, platform, runCommand, shortcuts]);

  const handleInput = useCallback((paneId: string, input: TerminalInput) => {
    if (!clientId || !hostState.canMutate) return;
    // The one place a keystroke becomes a request, so the one place the wait
    // for its echo can start.
    echoLagProbe.noteInput(paneId);
    const request = input.kind === "text" ? sendInput(clientId, paneId, input.data) : sendBinaryInput(clientId, paneId, input.data);
    void request.catch((error) => { if (clientIdRef.current === clientId) setStatus(String(error)); });
  }, [clientId, echoLagProbe, hostState.canMutate]);

  // Which workspace tmux sizes from is decided here and nowhere else, so it is
  // stated to the host as a fact rather than left to whichever event happened
  // to change it.
  useVisibleTerminalSession({
    activeSessionId,
    canMutate: hostState.canMutate,
    clientId,
    onStatus: setStatus,
    selectionAcknowledgement: hostSessionSelection,
    terminalEpoch,
    topologyGeneration: hostState.generation,
  });

  // The client size is computed from the tiled surface and from what a live
  // terminal turns pixels into. Both arrive here; neither is a pane's geometry.
  // `actualSize` is the other direction — what tmux settled on — and is the
  // only way the app can tell that another terminal took the size away from it.
  const actualWindowSize = useMemo(
    () => windowCellSize(snapshot.panes, activeWindowId),
    [activeWindowId, snapshot.panes],
  );
  const { onMeasurements, surfaceRef } = useClientResize({
    activeWindowId,
    actualSize: actualWindowSize,
    appFocused,
    canMutate: hostState.canMutate,
    clientId,
    onStatus: setStatus,
  });

  const beginDividerDrag = useCallback((event: React.PointerEvent<HTMLElement>, pane: Pane, axis: "horizontal" | "vertical") => {
    if (!hostState.canMutate) return;
    const target = event.currentTarget;
    const origin = axis === "horizontal" ? event.clientX : event.clientY;
    target.setPointerCapture(event.pointerId);
    target.onpointerup = (up) => {
      const delta = (axis === "horizontal" ? up.clientX : up.clientY) - origin;
      const bounds = target.parentElement?.getBoundingClientRect();
      const pixels = axis === "horizontal" ? bounds?.width ?? 0 : bounds?.height ?? 0;
      const cells = resizeCellsFromPixels(delta, pixels, axis === "horizontal" ? pane.width : pane.height);
      const kind = axis === "horizontal"
        ? (delta >= 0 ? "resizePaneRight" : "resizePaneLeft")
        : (delta >= 0 ? "resizePaneDown" : "resizePaneUp");
      if (Math.abs(delta) >= 4) void performAction({ kind, paneId: pane.id, resizeCells: cells });
      target.onpointerup = null;
    };
  }, [hostState.canMutate, performAction]);
  const focusTerminalPane = useCallback((pane: Pane) => {
    void shellNavigation.selectPane(pane, { kind: "silent" });
  }, [shellNavigation]);

  /**
   * Hands the keyboard back when an app tab stops covering the terminal.
   *
   * A pane focuses itself once, in its mount effect, and leaving a file tab
   * used to remount every pane — which is what put the caret back in the
   * terminal. The panes now stay mounted underneath, so nothing remounts and
   * nothing would focus: the covering surface unmounts and focus falls to the
   * document body. Only the uncovering transition is acted on; a plain window
   * switch still mounts fresh panes that focus themselves.
   */
  const appTabWasSelected = useRef(Boolean(selectedAppTab));
  useEffect(() => {
    if (selectedAppTab) {
      appTabWasSelected.current = true;
      return;
    }
    if (!appTabWasSelected.current) return;
    // Held, not dropped, until a pane is there to receive it: the tab can be
    // cleared a render before the window it returns to has an active pane.
    if (!activePane) return;
    appTabWasSelected.current = false;
    controllers.current.get(activePane.id)?.focus();
  }, [activePane, selectedAppTab]);

  const closeCombinedTab = (tab: CombinedTab, scope: HostScopeToken) => {
    // Nothing on the host to close yet; the create's own failure path
    // withdraws the placeholder.
    if (tab.kind === "pending") return;
    void runCommand("window.close", { kind: tab.kind === "app" ? "appTab" : "terminalTab", id: tab.id, scope });
  };

  /**
   * Closes a set of tabs, once the set is settled and any confirmation is past.
   *
   * The flush happens once for the whole set rather than per tab, and before
   * anything is closed: a set that cannot be saved must not lose half of itself
   * on the way to the failure message.
   */
  const closeTabSet = async (tabs: readonly CombinedTab[], scope: HostScopeToken) => {
    if (!sameHostConnection(scope, hostScopeRef.current)) {
      setStatus("Closing those tabs was cancelled because its host scope changed.");
      return;
    }
    try {
      await editorFlushRegistry.flushAll();
    } catch (error) {
      if (sameHostConnection(scope, hostScopeRef.current)) {
        setStatus(`Could not close those tabs because an editor did not save: ${String(error)}`);
      }
      return;
    }
    if (!sameHostConnection(scope, hostScopeRef.current)) return;
    for (const tab of tabs) {
      if (tab.kind !== "app") continue;
      const appTab = workspaceAppTabs.find((item) => item.id === tab.id);
      if (appTab) closeWorkspaceAppTab(appTab, scope);
    }
    for (const tab of tabs) {
      if (tab.kind !== "terminal") continue;
      const terminalWindow = windows.find((item) => item.id === tab.id);
      // No captured precondition: each close advances the topology generation,
      // so one stamped before the first would refuse every close after it.
      if (terminalWindow) await performAction({
        kind: "closeWindow", sessionId: terminalWindow.sessionId, windowId: terminalWindow.id, confirmed: true,
      });
      if (!sameHostConnection(scope, hostScopeRef.current)) return;
    }
  };

  /** Terminal windows in the set mean one dialog for the set; app tabs alone close on the spot. */
  const bulkCloseTabs = (tabs: CombinedTab[], scope: HostScopeToken) => {
    if (tabs.length === 0) return;
    if (tabs.some((tab) => tab.kind === "terminal")) setPendingBulkClose({ tabs, scope });
    else void closeTabSet(tabs, scope);
  };

  const openExplorerEntry = (entry: FileEntry, options: { preview: boolean }) => {
    if (!activeSession || !hostState.serverIdentity || !workspaceFiles.root || entry.kind === "directory"
      || (entry.kind === "symlink" && entry.targetKind !== "file")) return;
    const kind = /\.md(?:own)?$/i.test(entry.name) ? "markdown" as const : "file" as const;
    const session = activeSession;
    const serverIdentity = hostState.serverIdentity;
    const root = workspaceFiles.root;
    shellNavigation.selectLocalAppTab(session.id, activeWindowId, `file:${root.token}:${entry.path}`, () => {
      setAppState((current) => openFileTab(
        current, currentHostProfileId, serverIdentity, session, entry.path, kind, root, options,
      ));
    });
  };

  /** A preview tab stops being disposable the moment the user commits to it. */
  const pinOpenTab = (tabId: string) => setAppState((current) => pinAppTab(current, currentHostProfileId, tabId));

  const { mutateFile, startDownloadFlow } = useAppFileActions({
    canMutate: hostState.canMutate,
    client: fileClient,
    currentHostProfileId,
    recordTransfer: workspaceFiles.recordTransfer,
    refreshDirectory: workspaceFiles.refresh,
    root: workspaceFiles.root,
    scope: fileScope,
    setActiveDownloadStatus,
    setAppState,
    setStatus,
  });

  const moveCombinedTab = (tab: CombinedTab, direction: "left" | "right", scope: HostScopeToken) => {
    if (tab.kind === "pending") return;
    void runCommand(direction === "left" ? "window.moveLeft" : "window.moveRight", {
      kind: tab.kind === "app" ? "appTab" : "terminalTab",
      id: tab.id,
      scope,
    });
  };

  const updateShell = (update: Partial<ShellState>) =>
    setAppState((current) => ({ ...current, shell: { ...current.shell, ...update } }));

  const sidebarWidth = sidebarWidthForWindow(appState.shell.sidebarWidth, windowWidth);
  const panelWidth = panelWidthForWindow(appState.shell.panelWidth, windowWidth);
  // Derived, never stored: see `effectiveRails`.
  const { panelOpen, sidebarOpen } = effectiveRails(appState.shell, compactViewport);

  return <main
    className={[
      "shell",
      sidebarOpen ? "" : "sidebar-collapsed",
      panelOpen ? "panel-open" : "",
      compactViewport ? "compact" : "",
      platform === "mac" ? "platform-mac" : "platform-linux",
    ].filter(Boolean).join(" ")}
    style={{ ["--sidebar-width" as string]: `${sidebarWidth}px` }}
  >
    <TitleBar
      canMutate={hostState.canMutate}
      onBell={() => void runCommand("agents.jumpUnread")}
      onNewWorkspace={() => void runCommand("session.new")}
      onTogglePanel={() => void runCommand("view.togglePanel")}
      onToggleSidebar={() => void runCommand("view.toggleSidebar")}
      panelOpen={panelOpen}
      platform={platform}
      sidebarOpen={sidebarOpen}
      unread={unread}
      workspaceName={activeSession?.name}
    />
    <div className="shell-body">
      {/* Inside the body, and positioned over it: the strip is an overlay now,
          so the row it used to occupy no longer resizes every tmux window on
          the way in and out. `.shell-body` is its containing block. */}
      <DisconnectedStrip
        // `connectionDetail` only — never the general status line. The status
        // line carries whatever happened last, which during a disconnect is
        // usually an unrelated consequence ("Could not mark %117 hidden…"), and
        // this strip's job is to explain the connection. The full status stays in
        // the live region at the end of the shell.
        detail={connectionDetail}
        hasSnapshot={snapshot.sessions.length > 0}
        onOpenSettings={() => setSettingsOpen(true)}
        onReconnect={() => setConnectionEpoch((value) => value + 1)}
        phase={hostState.phase}
      />
      {sidebarOpen && <WorkspaceSidebar
        adapters={agentRuntime.adapters}
        agents={agentRows}
        agentSort={appState.shell.agentSort}
        agentsRatio={appState.shell.agentsSectionRatio}
        canMutate={hostState.canMutate}
        commandScope={currentHostScope}
        hookNotice={agentHostSetup.notice}
        hostLabel={hostLabel}
        latencyMs={latency?.milliseconds}
        onSetUpHost={agentHostSetup.offerable ? agentHostSetup.offer : undefined}
        onAgentsRatio={(ratio) => updateShell({ agentsSectionRatio: clampedAgentsRatio(ratio) })}
        maxWidth={Math.max(SIDEBAR_MIN_WIDTH, Math.floor(windowWidth / 3))}
        onWidth={(width) => updateShell({ sidebarWidth: sidebarWidthForWindow(width, windowWidth) })}
        width={sidebarWidth}
        onLaunchAgent={agentWorkflow.launch}
        onOpenSettings={() => setSettingsOpen(true)}
        onRenameAgent={(agent, scope) => {
          if (!sameHostConnection(scope, hostScopeRef.current)) return;
          setTextPrompt({
            title: "Rename agent",
            label: "Agent name",
            initialValue: agent.displayName,
            submit: (name) => {
              setTextPrompt(undefined);
              if (sameHostConnection(scope, hostScopeRef.current)) agentWorkflow.rename(agent, name);
            },
          });
        }}
        onResumeAgent={(agent, placement, scope) => {
          if (sameHostConnection(scope, hostScopeRef.current)) agentWorkflow.resume(agent, placement);
        }}
        onReviewHooks={agentWorkflow.reviewHooks}
        onSelectAgent={selectAgentRow}
        onSelectWorkspace={selectSession}
        onSortMode={(mode) => updateShell({ agentSort: mode })}
        onWorkspaceCommand={(session, commandId, scope) => void runCommand(commandId, { kind: "session", id: session.id, scope })}
        phase={hostState.phase}
        rows={sidebarRows}
        stateGlyphs={appState.shell.agentStateGlyphs}
        transport={connection.mode}
      />}
      <section className="workspace" aria-label={activeSession ? `Workspace ${activeSession.name}` : "Workspace"}>
        <TabStrip
          activeKey={activeCombinedTabKey}
          canMutate={hostState.canMutate && Boolean(activeSession)}
          canSplit={hostState.canMutate && Boolean(activePane) && !selectedAppTab}
          commandScope={currentHostScope}
          onClose={closeCombinedTab}
          onCloseOthers={(tab, scope) => bulkCloseTabs(tabsToCloseOthers(combinedTabs, tab.key), scope)}
          onCloseRight={(tab, scope) => bulkCloseTabs(tabsToCloseRight(combinedTabs, tab.key), scope)}
          onDownloadTab={(tab) => {
            const appTab = workspaceAppTabs.find((item) => item.id === tab.id);
            if (!appTab) return;
            // The tab's own root snapshot when it has one, exactly as the
            // surface reconstructs it; the live workspace root otherwise.
            const root = appTab.rootPath && appTab.rootToken
              ? {
                token: appTab.rootToken,
                path: appTab.rootPath,
                paneId: fileScope?.paneId ?? "",
                cwd: appTab.rootPath,
                gitWorktree: false,
                revision: "0",
              }
              : workspaceFiles.root;
            if (root) void startDownloadFlow({ path: appTab.resource, kind: "file" }, root);
          }}
          onMove={moveCombinedTab}
          onNewTerminal={() => void runCommand("window.new")}
          onPin={(tab) => pinOpenTab(tab.id)}
          onRenameTerminal={(tab, scope) => void runCommand("window.rename", { kind: "terminalTab", id: tab.id, scope })}
          onSelect={selectCombinedTab}
      stateGlyphs={appState.shell.agentStateGlyphs}
          onSplit={() => void runCommand("pane.splitRight")}
          tabs={combinedTabs}
        />
        <div
          aria-label={activeCombinedTabKey ? undefined : "Workspace content"}
          aria-labelledby={activeCombinedTabKey ? workspaceTabDomId(activeCombinedTabKey) : undefined}
          className="workspace-content"
          id={activeCombinedTabKey ? workspaceTabPanelDomId(activeCombinedTabKey) : undefined}
          role="tabpanel"
          tabIndex={0}
        >
          {/*
            Two layers, not a swap. The terminal layer keeps the active window's
            panes mounted while a file or diff tab is on top of it, so coming
            back is a repaint rather than a rebuild: no drain/serialize, no
            visibility round trip, no restore-from-cache, no reveal handshake.
            It is hidden with `visibility`, never `display`, so its box, xterm's
            fit and the tmux client measurement all stay valid underneath.

            The accepted trade-off: a covered pane is still `setTerminalVisibility(true)`
            and still streams output into a terminal nobody is looking at. That
            is deliberate and bounded — only the *active window's* panes are
            ever mounted, which is the same set that was live a moment ago.
          */}
          <div
            className={selectedAppTab ? "terminal-layer terminal-layer-covered" : "terminal-layer"}
            inert={selectedAppTab ? true : undefined}
          >
            <TerminalWorkspaceSurface
              activePane={activePane}
              activeWindow={activeWindow}
              appFocused={appFocused}
              beginDividerDrag={beginDividerDrag}
              clientId={clientId}
              controllers={controllers}
              focusPane={focusTerminalPane}
              onMeasurements={onMeasurements}
              grid={grid}
              handleInput={handleInput}
              hub={hub}
              mountedPanes={mountedPanes}
              paneAttention={agentRuntime.rollups.byPane}
              panes={panes}
              performAction={performAction}
              setStatus={setStatus}
              surfaceRef={surfaceRef}
              terminalTransferClient={terminalTransferClient}
              terminalTransferRegistry={terminalTransferRegistry}
              terminalTransferScope={terminalTransferScope}
            />
          </div>
          {/*
            The same trade the terminal layer makes, for documents. The last
            few selected file and diff tabs stay mounted and the unselected ones
            are covered, so switching back to one is a repaint: no second read
            of a file that is already in hand, no second Monaco, and no framed
            blank frame in between. `MOUNTED_APP_TAB_LIMIT` bounds what that
            costs; the first open of a tab still shows the frame, which is the
            only time it means anything.

            A covered tab is still live — it still watches its file, still
            revalidates and still autosaves. That is deliberate: a hidden tab
            can be dirty, and a dirty tab that stopped saving itself because it
            was not on screen would be the worse bargain.
          */}
          {mountedAppTabs.map((tab) => <div
            className={tab.id === selectedAppTab?.id ? "app-tab-layer" : "app-tab-layer app-tab-layer-covered"}
            inert={tab.id === selectedAppTab?.id ? undefined : true}
            key={appTabLayerKey(tab)}
          >
            <Suspense fallback={<AppTabFrame tab={tab} />}>{tab.kind === "gitDiff" ? <GitDiffSurface
              activeRoot={workspaceFiles.root}
              canWrite={hostState.canMutate}
              repositories={gitRepositories}
              onMessage={setStatus}
              scope={fileScope}
              tab={tab}
            /> : <AppTabSurface
              activeRoot={workspaceFiles.root}
              canWrite={hostState.canMutate}
              client={fileClient}
              onDownload={(path, kind, root) => void startDownloadFlow({ path, kind }, root)}
              onDirty={() => pinOpenTab(tab.id)}
              onStatus={setStatus}
              onViewMode={(viewMode) => setAppState((current) => setMarkdownViewMode(current, currentHostProfileId, tab.id, viewMode))}
              scope={fileScope}
              tab={tab}
            />}</Suspense>
          </div>)}
        </div>
      </section>
      {panelOpen && <AppRightPanel
        canMutate={hostState.canMutate}
        fileClient={fileClient}
        fileScope={fileScope}
        ignoredPaths={ignoredPaths}
        maxWidth={Math.max(PANEL_MIN_WIDTH, Math.floor(windowWidth / 2))}
        onDownload={async (intent) => { if (workspaceFiles.root) await startDownloadFlow(intent, workspaceFiles.root); }}
        onGitDiff={(entry, target) => {
          if (!activeSession || !hostState.serverIdentity || !workspaceFiles.root || !workspaceGit.status) return;
          const session = activeSession;
          const serverIdentity = hostState.serverIdentity;
          const root = workspaceFiles.root;
          const gitStatus = workspaceGit.status;
          shellNavigation.selectLocalAppTab(
            session.id,
            activeWindowId,
            `git:${gitStatus.repository.id}:${target}:${entry.path}`,
            () => setAppState((current) => openGitDiffTab(
              current, currentHostProfileId, serverIdentity, session, entry, target, gitStatus, root,
            )),
          );
        }}
        onMessage={setStatus}
        onMutate={mutateFile}
        onOpenFile={openExplorerEntry}
        onSurface={(surface) => void runCommand(surface === "files" ? "view.showFiles" : "view.showGit")}
        onWidth={(width) => updateShell({ panelWidth: panelWidthForWindow(width, windowWidth) })}
        surface={appState.shell.panelSurface}
        width={panelWidth}
        workspaceFiles={workspaceFiles}
        workspaceGit={workspaceGit}
      />}
    </div>

    <AppNoticeLayer
      agentDialog={agentWorkflow.dialog}
      agentSetupDialog={agentHostSetup.dialog}
      appStateRecovery={appStateRecovery}
      completedDownload={completedDownload}
      notice={notice}
      onClearDownload={() => setCompletedDownload(undefined)}
      onDismissNotice={() => setNotice(undefined)}
      onDownloadResult={(error) => { if (error) setStatus(error); }}
      onOfferDiscard={appRecovery.requestDiscard}
      onOfferRestore={appRecovery.restore}
      onProfileReset={() => setProfileResetConfirmation(true)}
      onStateReset={() => setAppStateResetConfirmation(true)}
      onTransferError={(error) => setStatus(String(error))}
      profileRecovery={profileRecovery}
      recoveryOffer={appRecovery.offer}
      terminalTransferClient={terminalTransferClient}
      terminalTransferRegistry={terminalTransferRegistry}
    />
    {settingsOpen && <SettingsDialog
      agentSetup={{
        available: agentHostSetup.offerable,
        connected: Boolean(agentScope),
        reports: agentHostSetup.reports,
        onSetUp: () => { setSettingsOpen(false); agentHostSetup.offer(); },
      }}
      connectionMode={connectionMode}
      deletableProfile={deletableProfile}
      helper={helperState}
      // An empty form with no saved host behind it, which is the one state the
      // picker can no longer reach on its own now that it lists saved hosts
      // only. SSH because a second local host is not a thing that exists.
      onAddHost={() => {
        setSelectedProfileId("");
        setSshTarget("");
        setSshConfigPath("");
        setConnectionMode("ssh");
      }}
      onClose={() => setSettingsOpen(false)}
      onConnect={() => { connect(); setSettingsOpen(false); }}
      onConnectionMode={(mode) => { setSelectedProfileId(""); setConnectionMode(mode); }}
      onDeleteProfile={() => void runCommand("host.delete")}
      onNotificationStatus={notificationPermissionStatus}
      // Not routed through `setStatus`: this is a diagnostic the user pressed a
      // button to get, and it belongs beside the button that produced it.
      onTestNotification={emitTestNotification}
      onProbeHelper={() => void probeHelper()}
      onProfile={selectProfile}
      onRequestHelperInstall={() => dispatchHelper({ type: "requestUpgrade" })}
      onShell={updateShell}
      onSounds={(preferences) => { setAgentSounds(preferences); saveAgentSoundPreferences(preferences); }}
      // The selection survives typing. It used to be cleared on every
      // keystroke, which made "correct this host's address" indistinguishable
      // from "add a host": Connect derived a fresh id from the new values and
      // saved a second entry for the same machine beside the one being
      // corrected. Editing the fields of a picked host now edits that host.
      onSshConfigPath={setSshConfigPath}
      onSshTarget={setSshTarget}
      profiles={profiles}
      remote={connection.mode === "ssh"}
      selectedProfileId={selectedProfileId}
      shell={appState.shell}
      sounds={agentSounds}
      sshConfigPath={sshConfigPath}
      sshTarget={sshTarget}
    />}
    {pendingBulkClose && <ConfirmationDialog
      confirmLabel="Close"
      destructive
      detail={bulkCloseDetail(pendingBulkClose.tabs)}
      onCancel={() => setPendingBulkClose(undefined)}
      onConfirm={() => {
        const pending = pendingBulkClose;
        setPendingBulkClose(undefined);
        void closeTabSet(pending.tabs, pending.scope);
      }}
      title={`Close ${pendingBulkClose.tabs.length} ${pendingBulkClose.tabs.length === 1 ? "tab" : "tabs"}?`}
    />}
    {workspaceSwitcherOpen && <WorkspaceSwitcher
      onClose={() => setWorkspaceSwitcherOpen(false)}
      onSelect={selectSession}
      rows={sidebarRows}
      stateGlyphs={appState.shell.agentStateGlyphs}
    />}
    <AppDialogLayer
      appRecoveryDiscard={appRecovery.dialog}
      appStateResetConfirmation={appStateResetConfirmation}
      commandContext={commandContext}
      confirmation={confirmation}
      helperState={helperState}
      hostDelete={hostDeleteConfirmation}
      onAppRecoveryDiscardCancel={appRecovery.cancelDiscard}
      onAppRecoveryDiscardConfirm={appRecovery.confirmDiscard}
      onAppStateResetCancel={() => setAppStateResetConfirmation(false)}
      onAppStateResetConfirm={() => {
        setAppStateResetConfirmation(false);
        void resetAppState().catch((error) => setStatus(`Could not reset saved shell state: ${String(error)}`));
      }}
      onConfirmationCancel={() => setConfirmation(undefined)}
      onConfirmationConfirm={(pending) => {
        setConfirmation(undefined);
        void performAction(pending.action, pending.precondition);
      }}
      onHelperCancel={() => dispatchHelper({ type: "cancelUpgrade" })}
      onHelperConfirm={() => {
        // An install — never an upgrade — is the dialog that says agent status
        // is part of what it sets up, so it is the only one that answers the
        // agent question. Recorded against the host being installed on, and
        // read again when that host's connection comes back.
        if (helperState.phase === "confirming" && helperState.operation === "install") {
          setPendingAgentAutoSetup(currentHostProfileId);
        }
        void confirmHelperInstall();
      }}
      onHostDeleteCancel={() => setHostDeleteConfirmation(undefined)}
      onHostDeleteConfirm={(profile) => {
        setHostDeleteConfirmation(undefined);
        deleteSelectedProfile(profile);
      }}
      onPaletteClose={() => setPaletteOpen(false)}
      onProfileResetCancel={() => setProfileResetConfirmation(false)}
      onProfileResetConfirm={() => {
        setProfileResetConfirmation(false);
        void invoke("reset_host_profiles").then(() => {
          const local: HostProfile = { id: "local", label: "Local", connection: { mode: "local" } };
          setProfiles([local]);
          setProfileRecovery(undefined);
          // As `switchHostProfile` does. Changing the connection without
          // dropping the host state leaves the previous host's live client
          // under the new host's name for as long as the bridge takes to tear
          // down.
          dispatchHost({ type: "reset" });
          setActiveSessionId(undefined);
          setActiveWindowId(undefined);
          setConnection(local.connection);
          setConnectionMode("local");
          setConnectionEpoch((value) => value + 1);
          setStatus("Recovered host profiles were reset to Local; the invalid original remains preserved.");
        }).catch((error) => setStatus(`Could not reset recovered host profiles: ${String(error)}`));
      }}
      onRunCommand={(id) => void runCommand(id)}
      onShortcutChange={(next) => setAppState((current) => ({ ...current, commands: { shortcutOverrides: next } }))}
      onShortcutClose={() => setShortcutEditorOpen(false)}
      onTextPromptCancel={() => setTextPrompt(undefined)}
      paletteOpen={paletteOpen}
      platform={platform}
      profileResetConfirmation={profileResetConfirmation}
      shortcuts={shortcuts}
      shortcutEditorOpen={shortcutEditorOpen}
      textPrompt={textPrompt}
    />
    {/* No second live region. The notice above *is* one — `role="alert"` for a
        problem, `role="status"` otherwise — and it carries every message that
        is not routine progress. A duplicate `sr-only` region holding the same
        text announced each refusal twice, and held the routine "Live" at rest,
        which put a second resting connection indicator in the accessibility
        tree beside the host row. Routine progress is deliberately invisible to
        sighted users; the host row and the disconnected strip carry connection
        state with accessible names of their own. */}
  </main>;
}
