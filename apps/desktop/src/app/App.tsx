import { invoke } from "@tauri-apps/api/core";
import { lazy, Suspense, useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import type { PendingTextPrompt } from "../commands/TextInputDialog";
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
import { clampedAgentsRatio, sidebarWidthForWindow, SIDEBAR_MIN_WIDTH, type HostSetupDecision, type ShellState } from "../features/shell/types";
import {
  combineWorkspaceTabs,
  closeAppTab,
  mountedTerminalPanes,
  openFileTab,
  openGitDiffTab,
  pinAppTab,
  selectAppTab,
  setMarkdownViewMode,
  shouldSurfaceAuthoritativeTerminal,
  type CombinedTab,
  type PendingShellTab,
} from "../features/shell/model";
import { useContextMenusOpen } from "../ui/ContextMenu";
import { TabStrip, workspaceTabDomId, workspaceTabPanelDomId } from "../features/workspaces/TabStrip";
import { WorkspaceSidebar } from "../features/workspaces/WorkspaceSidebar";
import { WorkspaceSwitcher } from "../features/workspaces/WorkspaceSwitcher";
import { inferHome, workspaceRows } from "../features/workspaces/workspaceRows";
import type { HostProfile, Pane } from "./types";
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
import { useAppFileActions } from "./useAppFileActions";
import { AppNoticeLayer } from "./AppNoticeLayer";
import { AppRightPanel } from "./AppRightPanel";

const AppTabSurface = lazy(() => import("../features/shell/AppTabSurface").then((module) => ({ default: module.AppTabSurface })));
const GitDiffSurface = lazy(() => import("../features/git/GitDiffSurface").then((module) => ({ default: module.GitDiffSurface })));

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
  const connectionController = useAppConnectionController({ agentClient, fileClient, gitClient, setStatus });
  const {
    activeSessionId, activeWindowId, appFocused, clientHostProfileId, clientId, clientIdRef, connection,
    connectionDetail, connectionMode, currentHostProfileId,
    currentHostScope, dispatchHost, hostScopeRef, hostState, hub, profileRecovery,
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
    activeSessionId, activeWindowId, appState, clientId, connection,
    currentHostProfileId, fileClient, generation: hostState.generation, gitRepositories,
    serverIdentity: hostState.serverIdentity, snapshot, terminalEpoch, windows,
  });
  const selectedAppTabRef = useRef(selectedAppTab);
  selectedAppTabRef.current = selectedAppTab;
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
    () => mountedTerminalPanes(snapshot.panes, activeWindowId, Boolean(selectedAppTab), Boolean(activeWindow?.zoomed)),
    [activeWindow?.zoomed, activeWindowId, selectedAppTab, snapshot.panes],
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
  const { commandContext, runCommand } = useShellCommands({
    activePane, activeSession, activeWindow, appState, canMutate: hostState.canMutate,

    closeAppTab: (tab, scope) => {
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
    },
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
    || Boolean(confirmation) || Boolean(textPrompt)
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
    const request = input.kind === "text" ? sendInput(clientId, paneId, input.data) : sendBinaryInput(clientId, paneId, input.data);
    void request.catch((error) => { if (clientIdRef.current === clientId) setStatus(String(error)); });
  }, [clientId, hostState.canMutate]);

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

  const closeCombinedTab = (tab: CombinedTab, scope: HostScopeToken) => {
    // Nothing on the host to close yet; the create's own failure path
    // withdraws the placeholder.
    if (tab.kind === "pending") return;
    void runCommand("window.close", { kind: tab.kind === "app" ? "appTab" : "terminalTab", id: tab.id, scope });
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
      branch={workspaceGit.status?.repository.headName}
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
    <div className="shell-body">
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
          {selectedAppTab ? <Suspense fallback={<p className="quiet-empty">Loading…</p>}>{selectedAppTab.kind === "gitDiff" ? <GitDiffSurface
            activeRoot={workspaceFiles.root}
            canWrite={hostState.canMutate}
            repositories={gitRepositories}
            onMessage={setStatus}
            scope={fileScope}
            tab={selectedAppTab}
            key={`${selectedAppTab.hostProfileId}\0${selectedAppTab.serverIdentity}\0${selectedAppTab.sessionId}\0${selectedAppTab.id}\0${selectedAppTab.gitRepositoryId}\0${selectedAppTab.gitPath}\0${selectedAppTab.gitTarget}`}
          /> : <AppTabSurface
            activeRoot={workspaceFiles.root}
            canWrite={hostState.canMutate}
            client={fileClient}
            onDownload={(path, kind, root) => void startDownloadFlow({ path, kind }, root)}
            onDirty={() => pinOpenTab(selectedAppTab.id)}
            onStatus={setStatus}
            onViewMode={(viewMode) => setAppState((current) => setMarkdownViewMode(current, currentHostProfileId, selectedAppTab.id, viewMode))}
            scope={fileScope}
            tab={selectedAppTab}
            key={`${selectedAppTab.hostProfileId}\0${selectedAppTab.serverIdentity}\0${selectedAppTab.sessionId}\0${selectedAppTab.id}\0${selectedAppTab.resource}`}
          />}</Suspense> : <TerminalWorkspaceSurface
            activePane={activePane}
            activeWindow={activeWindow}
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
          />}
        </div>
      </section>
      {panelOpen && <AppRightPanel
        canMutate={hostState.canMutate}
        fileClient={fileClient}
        fileScope={fileScope}
        ignoredPaths={ignoredPaths}
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
        surface={appState.shell.panelSurface}
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
      onSshConfigPath={(value) => { setSelectedProfileId(""); setSshConfigPath(value); }}
      onSshTarget={(value) => { setSelectedProfileId(""); setSshTarget(value); }}
      profiles={profiles}
      remote={connection.mode === "ssh"}
      selectedProfileId={selectedProfileId}
      shell={appState.shell}
      sounds={agentSounds}
      sshConfigPath={sshConfigPath}
      sshTarget={sshTarget}
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
      onHelperConfirm={() => void confirmHelperInstall()}
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
