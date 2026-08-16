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
import { TerminalTransferHistory } from "../features/terminal/TerminalTransferSurface";
import { useTerminalTransferRegistry } from "../features/terminal/terminalTransferRegistry";
import { abandonPerfSpan, openPerfSpan, type PanePaintSpan } from "../perf/probe";
import { requestTmuxAction, type TmuxAction } from "../features/tmux/actions";
import { requestReconciledTmuxAction } from "../features/tmux/actionReconciliation";
import { useAgentWorkflow } from "../features/agents/AgentHookWorkflow";
import { TauriAgentClient } from "../features/agents/api";
import { buildAgentRows, jumpTarget, unreadCount, type AgentListRow } from "../features/agents/agentsList";
import { loadAgentSoundPreferences, saveAgentSoundPreferences } from "../features/agents/sound";
import { emitTestNotification, notificationPermissionStatus } from "../features/agents/notifications";
import { agentHostIdentity } from "../features/agents/types";
import { useAgentHostSetup } from "../features/agents/useAgentHostSetup";
import { useAgentNotificationActivation, type PaneSurfaceResult } from "../features/agents/useAgentNotificationActivation";
import { useAgentRuntime } from "../features/agents/useAgentRuntime";
import { keyForScope, keyForTransferConnection, TauriFileWorkspaceClient } from "../features/files/api";
import { ExplorerTree } from "../features/files/ExplorerTree";
import { reconcileDownloadStatus, type ActiveDownloadStatus, type DownloadCompletion } from "../features/files/downloadStatus";
import { DownloadActions } from "../features/files/DownloadActions";
import { chooseDownloadDestination, type DownloadIntent } from "../features/files/downloadFlow";
import { ignoredPathsFromStatus } from "../features/files/ignoredPaths";
import type { ActiveRoot, DownloadRequest, FileEntry, FileMutation } from "../features/files/types";
import { TauriGitWorkspaceClient } from "../features/git/api";
import { GitSidebar } from "../features/git/GitSidebar";
import { DisconnectedStrip } from "../features/shell/DisconnectedStrip";
import { RightPanel } from "../features/shell/RightPanel";
import { SettingsDialog } from "../features/shell/SettingsDialog";
import { TitleBar } from "../features/shell/TitleBar";
import { emptyFocusHistory, pruneFocusHistory, stepFocus, visitFocus, type FocusHistory } from "../features/shell/focusHistory";
import { resetHostLatency, useHostLatency } from "../features/shell/hostLatency";
import { noticeDismissDelay, noticeForStatus, type StatusNotice } from "../features/shell/statusNotice";
import { helperConnectionKey, helperUpgradeReducer, initialHelperUpgradeState, type HelperInstallReport, type RemoteHelperProbe } from "../features/shell/helperUpgrade";
import { profileIdForSshConnection } from "../features/shell/hostProfiles";
import { sameHostConnection, sameHostScope, type HostScopeToken } from "../features/shell/hostScope";
import { useShellCommands } from "../features/shell/useShellCommands";
import { effectiveRails } from "../features/shell/responsiveShell";
import { usePersistedAppState } from "../features/shell/usePersistedAppState";
import { clampedAgentsRatio, sidebarWidthForWindow, SIDEBAR_MIN_WIDTH, type HostSetupDecision, type ShellState } from "../features/shell/types";
import {
  combineWorkspaceTabs,
  discardServerAppState,
  mountedTerminalPanes,
  openFileTab,
  openGitDiffTab,
  pinAppTab,
  reconcileWorkspaceIdentity,
  recoverableAppTabCount,
  recoverAppTabsFromPreviousServer,
  relocateFileTabs,
  selectAppTab,
  setMarkdownViewMode,
  shouldSurfaceAuthoritativeTerminal,
  shellNavigationMode,
  type CombinedTab,
} from "../features/shell/model";
import { useContextMenusOpen } from "../ui/ContextMenu";
import { SurfaceError } from "../ui/SurfaceError";
import { TabStrip, workspaceTabDomId, workspaceTabPanelDomId } from "../features/workspaces/TabStrip";
import { WorkspaceSidebar } from "../features/workspaces/WorkspaceSidebar";
import { WorkspaceSwitcher } from "../features/workspaces/WorkspaceSwitcher";
import { inferHome, workspaceRows } from "../features/workspaces/workspaceRows";
import type { ConnectionSpec, HostProfile, Pane, PersistedProfiles } from "./types";
import { resolveTerminalDestination } from "./paneRouting";
import { requestActiveWindow } from "./windowSelection";
import { useAppConnectionController } from "./useAppConnectionController";
import { useClientResize } from "./useClientResize";
import { useVisibleTerminalSession } from "./useVisibleTerminalSession";
import { windowCellSize } from "../features/terminal/clientSize";
import { useWorkspaceDomainController } from "./useWorkspaceDomainController";
import { AppDialogLayer } from "./AppDialogLayer";
import { TerminalWorkspaceSurface } from "./TerminalWorkspaceSurface";

const AppTabSurface = lazy(() => import("../features/shell/AppTabSurface").then((module) => ({ default: module.AppTabSurface })));
const GitDiffSurface = lazy(() => import("../features/git/GitDiffSurface").then((module) => ({ default: module.GitDiffSurface })));

/**
 * Actions whose perceived completion is a pane painting. Phase 12 budgets these
 * as "action to interactive pane", so the instrumentation spans have to start
 * at the action and end at the paint rather than at the tmux ack.
 */
const INTERACTION_SPAN_BY_ACTION: Partial<Record<TmuxAction["kind"], PanePaintSpan>> = {
  createSession: "create.workspace",
  createWindow: "create.tab",
  selectWindow: "window.switch",
  splitPaneDown: "pane.split",
  splitPaneRight: "pane.split",
};

/** Below this the sidebar overlays the terminal instead of taking space. */
const COMPACT_VIEWPORT_QUERY = "(max-width: 880px)";

export function App() {
  const [status, setStatus] = useState("Discovering local tmux…");
  const [activeDownloadStatus, setActiveDownloadStatus] = useState<ActiveDownloadStatus>();
  const agentClient = useMemo(() => new TauriAgentClient(), []);
  const fileClient = useMemo(() => new TauriFileWorkspaceClient(), []);
  const gitClient = useMemo(() => new TauriGitWorkspaceClient(), []);
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
  const [compactViewport, setCompactViewport] = useState(() => window.matchMedia?.(COMPACT_VIEWPORT_QUERY).matches ?? false);
  const [windowWidth, setWindowWidth] = useState(() => window.innerWidth || 1280);
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
  const deleteSelectedProfile = (profile: HostProfile) => {
    void invoke<PersistedProfiles>("delete_host_profile", { profileId: profile.id }).then((saved) => {
      // The store's surviving list, not a locally filtered guess at it.
      setProfiles(saved.profiles);
      setSelectedProfileId("");
      setStatus(`Deleted the saved host ${profile.label}.`);
    }).catch((error) => setStatus(`Could not delete the saved host ${profile.label}: ${String(error)}`));
  };
  const [shortcutEditorOpen, setShortcutEditorOpen] = useState(false);
  const [confirmation, setConfirmation] = useState<PendingTmuxConfirmation>();
  const [textPrompt, setTextPrompt] = useState<PendingTextPrompt>();
  const [appStateResetConfirmation, setAppStateResetConfirmation] = useState(false);
  const [appRecoveryDiscardConfirmation, setAppRecoveryDiscardConfirmation] = useState(false);
  const [pendingAppRecovery, setPendingAppRecovery] = useState<{ hostProfileId: string; previousServerIdentity: string; currentServerIdentity: string; count: number; scope: HostScopeToken }>();
  const [completedDownload, setCompletedDownload] = useState<DownloadCompletion & { noticeId?: number }>();
  const downloadPickerOpen = useRef(false);
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
  const [notice, setNotice] = useState<StatusNotice>();
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

  // Anything the app says that is not routine progress becomes a visible,
  // dismissible notice. Without this the whole status channel — every refused
  // action, every unreachable agent, the client-size refusal that is designed
  // to be loud — reached only the screen-reader live region.
  const noticeSequence = useRef(0);
  useEffect(() => {
    const next = noticeForStatus(status, (noticeSequence.current += 1));
    setNotice(next);
    // The completion's Open/reveal buttons belong to the notice announcing it
    // and to no other. The message is matched once, here, at the moment the
    // notice is minted — after which the two are joined by the notice's id, so
    // nothing downstream re-derives the association from user-facing prose.
    // (`noticeForStatus` trims, hence the trim.) Anything else drops the
    // record rather than holding a finished download's destination for the
    // rest of the session.
    setCompletedDownload((current) => current && next && current.message.trim() === next.message
      ? { ...current, noticeId: next.id }
      : undefined);
    if (!next) return;
    const delay = noticeDismissDelay(next);
    if (delay === undefined) return;
    const timer = window.setTimeout(() => setNotice((current) => current?.id === next.id ? undefined : current), delay);
    return () => window.clearTimeout(timer);
  }, [status]);
  // A new bridge is a new link; the last one's measured round-trip describes
  // nothing about it.
  useEffect(() => { resetHostLatency(); }, [clientId]);

  // Width is observed, never saved. What a narrow window does to the rails is
  // decided at render time by `effectiveRails`; writing it into the preferences
  // meant one narrow moment overwrote the user's arrangement permanently.
  useEffect(() => {
    if (!window.matchMedia) return;
    const query = window.matchMedia(COMPACT_VIEWPORT_QUERY);
    setCompactViewport(query.matches);
    const handleChange = (event: MediaQueryListEvent) => setCompactViewport(event.matches);
    query.addEventListener("change", handleChange);
    return () => query.removeEventListener("change", handleChange);
  }, []);

  // The sidebar may be dragged wider, but never past a third of the window, so
  // the terminal keeps its share when the window shrinks under a wide sidebar.
  useEffect(() => {
    const onResize = () => setWindowWidth(window.innerWidth || 1280);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  const lastReconciledIdentity = useRef<{ hostProfileId: string; serverIdentity?: string } | undefined>(undefined);
  useEffect(() => {
    const previous = lastReconciledIdentity.current;
    setAppState((current) => {
      if (previous?.serverIdentity && hostState.serverIdentity
        && previous.hostProfileId === currentHostProfileId
        && previous.serverIdentity !== hostState.serverIdentity) {
        const count = recoverableAppTabCount(current, currentHostProfileId, previous.serverIdentity, snapshot.sessions);
        if (count > 0) setPendingAppRecovery({ hostProfileId: currentHostProfileId, previousServerIdentity: previous.serverIdentity, currentServerIdentity: hostState.serverIdentity, count, scope: currentHostScope });
      }
      return reconcileWorkspaceIdentity(current, currentHostProfileId, hostState.serverIdentity, snapshot.sessions);
    });
    lastReconciledIdentity.current = { hostProfileId: currentHostProfileId, serverIdentity: hostState.serverIdentity };
  }, [currentHostProfileId, hostState.serverIdentity, snapshot.sessions]);

  useEffect(() => {
    if (pendingAppRecovery && !sameHostScope(pendingAppRecovery.scope, currentHostScope)) setPendingAppRecovery(undefined);
  }, [currentHostScope.connectionEpoch, currentHostScope.connectionKey, currentHostScope.generation, currentHostScope.hostProfileId, currentHostScope.serverIdentity, pendingAppRecovery]);

  const {
    activePane, activeSession, activeWindow, fileScope, panes, selectedAppTab,
    terminalTransferScope, workspaceAppTabs, workspaceFiles, workspaceGit,
  } = useWorkspaceDomainController({
    activeSessionId, activeWindowId, appState, clientId, connection,
    currentHostProfileId, fileClient, generation: hostState.generation, gitClient,
    serverIdentity: hostState.serverIdentity, snapshot, terminalEpoch, windows,
  });
  /** Where the files controller and the git controller meet; the rule itself is `ignoredPathsFromStatus`. */
  const ignoredPaths = useMemo(() => ignoredPathsFromStatus(workspaceGit.status), [workspaceGit.status]);

  useEffect(() => {
    if (!activeDownloadStatus) return;
    const reconciled = reconcileDownloadStatus(status, activeDownloadStatus, workspaceFiles.transfers);
    if (reconciled.status !== status) setStatus(reconciled.status);
    if (reconciled.active !== activeDownloadStatus) setActiveDownloadStatus(reconciled.active);
    if (reconciled.completion) setCompletedDownload(reconciled.completion);
  }, [activeDownloadStatus, status, workspaceFiles.transfers]);
  const performAction = useCallback(async (
    action: TmuxAction,
    capturedPrecondition?: { serverIdentity: string; generation: number },
  ) => {
    if (!clientId || !hostState.canMutate || !hostState.serverIdentity) {
      setStatus("This action is unavailable until the authoritative connection is live.");
      return undefined;
    }
    // The user's wait for a create or a split ends when a pane paints, not when
    // tmux acks; the pane that paints closes this span (see TerminalPane).
    const paneSpan = INTERACTION_SPAN_BY_ACTION[action.kind];
    if (paneSpan) openPerfSpan(paneSpan);
    try {
      const result = await requestReconciledTmuxAction({
        clientId,
        action,
        capturedPrecondition,
        initialScope: hostScopeRef.current,
        currentScope: () => hostScopeRef.current,
      });
      setStatus("Waiting for authoritative tmux state…");
      return result;
    } catch (error) {
      if (paneSpan) abandonPerfSpan(paneSpan);
      setStatus(String(error));
      return undefined;
    }
  }, [clientId, hostState.canMutate, hostState.generation, hostState.serverIdentity]);

  const surfacePaneDestination = useCallback(async (target: Pane, source: string, successMessage?: string): Promise<PaneSurfaceResult> => {
    const scope = hostScopeRef.current;
    const session = snapshotRef.current.sessions.find((item) => item.id === target.sessionId);
    if (!session || !clientId || !hostState.canMutate || !hostState.serverIdentity) {
      const error = new Error(`${source} destination is no longer available.`);
      setStatus(error.message);
      return { ok: false, error };
    }
    try {
      // `focusPane` is `select-pane`, which selects a pane *within* its window
      // and leaves the session's active window untouched. The app then derives
      // its active tab from tmux's own `window_active` flag via
      // `resolveActiveWindowId`, so a destination in a non-active window landed
      // on the right workspace and the wrong terminal — the exact case a
      // notification exists for (M10-E061). Select the window first when it is
      // not already active, chaining the generation the first action returns so
      // the second is not rejected as stale.
      let generation = hostState.generation;
      const targetWindowIsActive = snapshotRef.current.windows.some(
        (item) => item.id === target.windowId && item.active,
      );
      if (!targetWindowIsActive) {
        const selected = await requestTmuxAction(clientId, { kind: "selectWindow", sessionId: target.sessionId, windowId: target.windowId }, {
          serverIdentity: hostState.serverIdentity,
          generation,
        });
        generation = selected.topologyGeneration;
      }
      await requestTmuxAction(clientId, { kind: "focusPane", sessionId: target.sessionId, windowId: target.windowId, paneId: target.id }, {
        serverIdentity: hostState.serverIdentity,
        generation,
      });
    } catch (error) {
      setStatus(String(error));
      return { ok: false, error };
    }
    // `sameHostConnection`, not `sameHostScope`. The guard exists to catch the
    // connection being replaced underneath a focus request — a profile switch,
    // a reconnect, a different tmux server. `sameHostScope` also compares the
    // topology generation, and the two actions just performed *always* bump it,
    // so that comparison could never hold: tmux moved to the agent's pane and
    // the app then refused to follow it, leaving the sidebar and the terminal
    // pointing at different workspaces. Measured against the real server: the
    // click selected window 5 / pane %120 while the app stayed on the previous
    // workspace.
    if (!sameHostConnection(scope, hostScopeRef.current)) return { ok: false, error: new Error("authoritative connection changed while focusing") };
    setAppState((current) => selectAppTab(current, currentHostProfileId, hostState.serverIdentity!, session, undefined));
    setActiveSessionId(target.sessionId);
    setActiveWindowId(target.windowId);
    setStatus(successMessage ? `${successMessage} Focus request accepted.` : `${source} focus request accepted for ${target.sessionId}/${target.windowId}/${target.id}.`);
    window.requestAnimationFrame(() => controllers.current.get(target.id)?.focus());
    return { ok: true };
  }, [clientId, currentHostProfileId, hostState.canMutate, hostState.generation, hostState.serverIdentity, setAppState]);

  // `clientHostProfileId === currentHostProfileId` is not redundant: the profile
  // id follows the connection spec immediately and the client follows it an
  // effect later, so a render that changes hosts commits with one host's name
  // and another's live client. There is no scope during that window rather
  // than a scope naming two machines — which is what let a write land on the
  // host the user had not answered about (M13-E004).
  const agentScope = useMemo(() => clientId && clientHostProfileId === currentHostProfileId
    && hostState.serverIdentity && hostState.canMutate ? {
    clientId,
    hostProfileId: currentHostProfileId,
    serverIdentity: hostState.serverIdentity,
    topologyGeneration: hostState.generation,
    connectionEpoch: terminalEpoch,
  } : undefined, [clientHostProfileId, clientId, currentHostProfileId, hostState.canMutate, hostState.generation, hostState.serverIdentity, terminalEpoch]);
  const notificationActivation = useAgentNotificationActivation({
    agentClient,
    agentScope,
    connected: hostState.canMutate && Boolean(hostState.serverIdentity),
    connectionEpoch: terminalEpoch,
    currentHostProfileId,
    focusedPaneId: activePane?.id,
    profiles,
    snapshot,
    requestReconnect: () => setConnectionEpoch((value) => value + 1),
    setStatus,
    surfacePaneDestination,
    switchHostProfile: (profile) => {
      const targetConnection: ConnectionSpec = profile.connection.mode === "ssh"
        ? { ...profile.connection, profileId: profile.connection.profileId || profile.id }
        : profile.connection;
      dispatchHost({ type: "reset" });
      setActiveSessionId(undefined);
      setActiveWindowId(undefined);
      setConnection(targetConnection);
      setConnectionMode(targetConnection.mode);
      if (targetConnection.mode === "ssh") {
        setSshTarget(targetConnection.target);
        setSshConfigPath(targetConnection.configPath ?? "");
      }
    },
  });
  const agentFocus = useMemo(() => ({
    hostProfileId: currentHostProfileId,
    serverIdentity: hostState.serverIdentity,
    sessionId: activeSessionId,
    windowId: activeWindowId,
    paneId: activePane?.id,
    appFocused,
    terminalVisible: !selectedAppTab,
    automaticSeen: notificationActivation.automaticSeen,
  }), [activePane?.id, activeSessionId, activeWindowId, appFocused, currentHostProfileId, hostState.serverIdentity, notificationActivation.automaticSeen, selectedAppTab]);
  const agentRuntime = useAgentRuntime({ client: agentClient, scope: agentScope, focus: agentFocus, soundPreferences: agentSounds, onStatus: setStatus });

  // The host every consent-bearing hook request is bound to: the profile the
  // decision is remembered under, and the connection it is checked against.
  //
  // `appStateRecovery` is part of it. A state file the app could not read is
  // write-frozen until the user resets it, so an answer given in that mode
  // reaches memory and nothing else — and a host whose answer cannot be kept is
  // not a host anything here may write. Both doors out of the setup prompt run
  // through this value, including the "Review exact changes…" one.
  const agentHost = useMemo(() => {
    const identity = agentHostIdentity(agentScope);
    return agentScope && identity && appStateRecovery === undefined
      ? { profileId: agentScope.hostProfileId, identity }
      : undefined;
  }, [agentScope, appStateRecovery]);
  const recordHostSetupDecision = useCallback((hostProfileId: string, decision: HostSetupDecision) => {
    setAppState((current) => ({
      ...current,
      hostSetup: { ...current.hostSetup, [hostProfileId]: decision },
    }));
  }, [setAppState]);
  const agentWorkflow = useAgentWorkflow({
    launchContext: activeSession && activeWindow && activePane && workspaceFiles.root
      ? { sessionId: activeSession.id, windowId: activeWindow.id, paneId: activePane.id, root: workspaceFiles.root }
      : undefined,
    host: agentHost,
    onHooksChanged: (action, hostProfileId, hostIdentity) => {
      agentRuntime.refreshSnapshot();
      // Installing through the exact-diff review *is* consent for this host,
      // and removing is withdrawing it. Recording only one of the two left a
      // user who took the review door with no decision at all: the one-time
      // prompt could re-raise, and the tmux naming was never asserted.
      //
      // Against the host the review named, which is the host that was written.
      recordHostSetupDecision(hostProfileId, action === "install" ? "accepted" : "declined");
      if (action === "uninstall") {
        void agentRuntime.removeHostNaming(hostIdentity).catch((cause) => setStatus(String(cause)));
      }
    },
    onModalChange: setAgentModalOpen,
    onStatus: setStatus,
    runtime: agentRuntime,
  });
  const hostLabel = connection.mode === "local" ? "local" : connection.target;
  const agentHostSetup = useAgentHostSetup({
    adapters: agentRuntime.adapters,
    applyHooks: agentRuntime.applyHooks,
    applyHostNaming: agentRuntime.applyHostNaming,
    connected: Boolean(agentScope),
    decision: appState.hostSetup[currentHostProfileId],
    decisionsArePersistable: appStateRecovery === undefined,
    hostIdentity: agentHost?.identity,
    hostLabel,
    hostProfileId: currentHostProfileId,
    onStatus: setStatus,
    openReview: agentWorkflow.openHookReview,
    recordDecision: recordHostSetupDecision,
    refreshWiring: agentRuntime.refreshSnapshot,
    reviewHooks: agentRuntime.reviewHooks,
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

  const combinedTabs = useMemo(
    () => combineWorkspaceTabs(windows, workspaceAppTabs, agentRuntime.rollups.byWindow),
    [agentRuntime.rollups.byWindow, windows, workspaceAppTabs],
  );
  const activeCombinedTabKey = selectedAppTab ? `app:${selectedAppTab.id}` : activeWindow ? `terminal:${activeWindow.id}` : undefined;
  const grid = windowGrid(panes);
  const mountedPanes = mountedTerminalPanes(snapshot.panes, activeWindowId, Boolean(selectedAppTab), Boolean(activeWindow?.zoomed));
  const lastAuthoritativeWindow = useRef(new Map<string, string>());

  useEffect(() => {
    const authoritative = windows.find((window) => window.active)?.id;
    const identityKey = activeSession && hostState.serverIdentity
      ? `${currentHostProfileId}\0${hostState.serverIdentity}\0${activeSession.id}`
      : undefined;
    const previous = identityKey ? lastAuthoritativeWindow.current.get(identityKey) : undefined;
    if (shouldSurfaceAuthoritativeTerminal(Boolean(selectedAppTab), activeSession?.id, previous, activeSession?.id, authoritative) && activeSession) {
      setAppState((current) => selectAppTab(current, currentHostProfileId, hostState.serverIdentity!, activeSession, undefined));
    }
    if (identityKey && authoritative) lastAuthoritativeWindow.current.set(identityKey, authoritative);
  }, [activeSession, currentHostProfileId, hostState.serverIdentity, selectedAppTab, windows]);

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
    if (target) void performAction({ kind: "focusPane", paneId: target.id, windowId: target.windowId, sessionId: target.sessionId });
  }, [activePane, panes, performAction]);

  const selectSession = useCallback((sessionId: string) => {
    notificationActivation.clearNotificationFocusGuard();
    if (sessionId === activeSessionId) return;
    if (shellNavigationMode(hostState.canMutate) === "cached") {
      setActiveSessionId(sessionId);
      setStatus("Viewing the last known workspace. Writes remain frozen.");
      return;
    }
    void performAction({ kind: "selectSession", sessionId }).then((accepted) => {
      if (accepted) setActiveSessionId(sessionId);
    });
  }, [activeSessionId, hostState.canMutate, notificationActivation, performAction]);

  const selectWindow = useCallback((windowId: string) => {
    notificationActivation.clearNotificationFocusGuard();
    if (activeSession && hostState.serverIdentity) setAppState((current) => selectAppTab(current, currentHostProfileId, hostState.serverIdentity!, activeSession, undefined));
    if (shellNavigationMode(hostState.canMutate) === "cached") {
      setActiveWindowId(windowId);
      setStatus("Viewing the last known terminal tab. Writes remain frozen.");
      return;
    }
    void requestActiveWindow(windows, activeWindowId, windowId, performAction, setActiveWindowId);
  }, [activeSession, activeWindowId, currentHostProfileId, hostState.canMutate, hostState.serverIdentity, notificationActivation, performAction, setAppState, windows]);

  const selectCombinedTab = useCallback((tab: CombinedTab) => {
    if (tab.kind === "terminal") selectWindow(tab.id);
    // No status: the tab the user asked for is now the tab on screen. The
    // message that used to be written here reached nobody either way — the
    // notice is the channel's only reader and it has classified "Opened …" as
    // routine since it was written.
    else if (activeSession && hostState.serverIdentity) {
      setAppState((current) => selectAppTab(current, currentHostProfileId, hostState.serverIdentity!, activeSession, tab.id));
    }
  }, [activeSession, currentHostProfileId, hostState.serverIdentity, selectWindow, setAppState]);

  const selectAgentRow = useCallback((row: AgentListRow) => {
    notificationActivation.clearNotificationFocusGuard();
    if (!row.agent.paneId) return setStatus(`Agent ${row.agent.displayName} has no exact pane match; navigation is unavailable.`);
    const destination = resolveTerminalDestination(snapshot.panes, row.agent.paneId);
    if (destination.kind === "unavailable") return setStatus(`Agent destination ${row.agent.displayName} is no longer available: ${destination.reason}.`);
    void surfacePaneDestination(destination.pane, `Agent ${row.agent.displayName}`);
  }, [notificationActivation, snapshot.panes, surfacePaneDestination]);

  // What the Explorer, Git and the agents list currently offer for the row the
  // user last pointed at — the palette's only way to name a row.
  const rowCommands = useRowCommands();
  const { commandContext, runCommand } = useShellCommands({
    activePane, activeSession, activeWindow, appState, canMutate: hostState.canMutate,

    combinedTabs, controllers, currentHostProfileId, deletableHostProfile: deletableProfile,
    focusDirection, generation: hostState.generation, hostScope: currentHostScope,
    isHostScopeCurrent: (scope) => sameHostConnection(scope, hostScopeRef.current),
    jumpToUnreadAgent: () => {
      const target = jumpTarget(agentRows);
      if (!target) return setStatus("No agent is waiting on you.");
      selectAgentRow(target);
    },
    performAction, requestHostProfileDelete: setHostDeleteConfirmation, rowCommands, selectedAppTab,
    selectCreatedSession: (sessionId) => { setActiveSessionId(sessionId); setActiveWindowId(undefined); },
    // Deliberately not `requestActiveWindow`: that one looks the window up in
    // the current snapshot first, and a window one round trip old is not in it
    // yet. A real `select-window` is also what makes the *next* snapshot agree
    // — the app mirrors tmux's active flag, so anything only set locally here
    // would be overwritten the moment the snapshot arrived. The generation is
    // chained from the create for the same reason `surfacePaneDestination`
    // chains its own: the create already moved the topology.
    selectCreatedWindow: (sessionId, windowId, generation) => {
      notificationActivation.clearNotificationFocusGuard();
      const scope = hostScopeRef.current;
      const identity = hostState.serverIdentity;
      if (!identity) return;
      void performAction({ kind: "selectWindow", sessionId, windowId }, { serverIdentity: identity, generation })
        .then((accepted) => {
          // Re-checked after the round trip, not only before it: the guard is
          // there to catch the connection being replaced mid-flight, which is
          // precisely what can happen while this is in the air.
          if (!accepted || !sameHostConnection(scope, hostScopeRef.current)) return;
          // `snapshotRef`, not the render's `snapshot`: the workspace may have
          // been renamed or closed while this was outstanding.
          const session = snapshotRef.current.sessions.find((item) => item.id === sessionId);
          if (session) setAppState((current) => selectAppTab(current, currentHostProfileId, identity, session, undefined));
          setActiveWindowId(windowId);
        });
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
    || agentModalOpen || agentHostSetup.open || appStateResetConfirmation || appRecoveryDiscardConfirmation
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

  const beginDividerDrag = (event: React.PointerEvent<HTMLElement>, pane: Pane, axis: "horizontal" | "vertical") => {
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
  };

  const closeCombinedTab = (tab: CombinedTab) => {
    void runCommand("window.close", { kind: tab.kind === "app" ? "appTab" : "terminalTab", id: tab.id });
  };

  const openExplorerEntry = (entry: FileEntry, options: { preview: boolean }) => {
    if (!activeSession || !hostState.serverIdentity || !workspaceFiles.root || entry.kind === "directory"
      || (entry.kind === "symlink" && entry.targetKind !== "file")) return;
    const kind = /\.md(?:own)?$/i.test(entry.name) ? "markdown" as const : "file" as const;
    setAppState((current) => openFileTab(
      current,
      currentHostProfileId,
      hostState.serverIdentity!,
      activeSession,
      entry.path,
      kind,
      workspaceFiles.root!,
      options,
    ));
  };

  /** A preview tab stops being disposable the moment the user commits to it. */
  const pinOpenTab = (tabId: string) => setAppState((current) => pinAppTab(current, currentHostProfileId, tabId));

  const mutateFile = async (mutation: FileMutation) => {
    if (!fileScope || !workspaceFiles.root || !hostState.canMutate) throw new Error("File changes are unavailable while the host is read-only.");
    const mutationScope = fileScope;
    const mutationHostProfileId = currentHostProfileId;
    const mutationRoot = workspaceFiles.root;
    try {
      await fileClient.mutate(mutationScope, mutationRoot, mutation);
      if (mutation.kind === "rename" || mutation.kind === "move") {
        setAppState((current) => relocateFileTabs(
          current,
          mutationHostProfileId,
          mutationScope.serverIdentity,
          mutationRoot.path,
          mutation.path,
          mutation.destination,
        ));
      }
      const directory = "parent" in mutation
        ? mutation.parent
        : mutation.path.slice(0, mutation.path.lastIndexOf("/")) || mutationRoot.path;
      // No toast: the tree redraws with the rename, the new file, or the row
      // gone. The failure below is the part nothing else on screen would say.
      workspaceFiles.refresh(directory);
    } catch (error) {
      setStatus(String(error));
      throw error;
    }
  };

  const startDownload = async (request: DownloadRequest, downloadRoot: ActiveRoot = workspaceFiles.root!) => {
    if (!fileScope || !downloadRoot) throw new Error("Downloads require a live file host.");
    try {
      const transfer = await fileClient.startDownload(fileScope, downloadRoot, request);
      workspaceFiles.recordTransfer(transfer);
      const banner = `Download ${transfer.state}: ${request.path}`;
      setActiveDownloadStatus({ id: transfer.id, path: request.path, banner });
      setStatus(banner);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      workspaceFiles.recordTransfer({
        id: crypto.randomUUID(), scopeKey: keyForTransferConnection(fileScope), path: request.path, destination: request.destination, kind: request.kind,
        state: "failed", outcome: "notPublished", failureKind: "transfer", completedBytes: "0", filesCompleted: "0", error: message,
      });
      setStatus(message);
    }
  };

  /**
   * The whole download gesture: the OS save panel, then the transfer. There is
   * no in-app step, so cancelling the panel ends it with nothing said.
   */
  const startDownloadFlow = async (intent: DownloadIntent, downloadRoot: ActiveRoot) => {
    // Three call sites invoke this fire-and-forget, and the in-app modal that
    // used to serialize them is gone — without this, two quick downloads open
    // two save panels.
    if (downloadPickerOpen.current) return;
    downloadPickerOpen.current = true;
    const chosen = await chooseDownloadDestination(intent)
      .catch((error) => { setStatus(`Could not open the save panel: ${String(error)}`); return undefined; })
      .finally(() => { downloadPickerOpen.current = false; });
    if (!chosen) return;
    // `overwrite`, not `rename`: the default name the panel opened with was
    // already unique, so reaching an existing file means the user aimed at one
    // and answered the panel's own Replace prompt. Where the panel could not
    // have asked — a folder archive the backend will rename to `.tar` — refuse
    // instead, because nothing may be replaced without being confirmed.
    await startDownload({
      path: intent.path,
      kind: intent.kind,
      destination: chosen.destination,
      collision: chosen.panelConfirmed ? "overwrite" : "fail",
    }, downloadRoot);
  };

  const moveCombinedTab = (tab: CombinedTab, direction: "left" | "right") => {
    void runCommand(direction === "left" ? "window.moveLeft" : "window.moveRight", {
      kind: tab.kind === "app" ? "appTab" : "terminalTab",
      id: tab.id,
    });
  };

  const selectProfile = (profile: HostProfile | undefined) => {
    setSelectedProfileId(profile?.id ?? "");
    if (!profile) return;
    setConnectionMode(profile.connection.mode);
    if (profile.connection.mode === "ssh") {
      setSshTarget(profile.connection.target);
      setSshConfigPath(profile.connection.configPath ?? "");
    }
  };

  const probeHelper = async () => {
    if (connection.mode !== "ssh") return;
    const scope = currentHostScope;
    const connectionKey = helperConnectionKey(connection);
    dispatchHelper({ type: "probe", connectionKey });
    try {
      const probe = await invoke<RemoteHelperProbe>("probe_remote_helper", { connection });
      if (!sameHostScope(scope, hostScopeRef.current)) return;
      dispatchHelper({ type: "probeSucceeded", connectionKey, probe });
    } catch (error) {
      if (!sameHostScope(scope, hostScopeRef.current)) return;
      dispatchHelper({ type: "probeFailed", connectionKey, message: String(error) });
    }
  };

  const confirmHelperInstall = async () => {
    if (connection.mode !== "ssh" || helperState.phase !== "confirming"
      || helperState.connectionKey !== helperConnectionKey(connection)) return;
    const connectionKey = helperState.connectionKey;
    const scope = currentHostScope;
    dispatchHelper({ type: "upgrade" });
    try {
      const report = await invoke<HelperInstallReport>("install_remote_helper", {
        connection,
        allowUpgrade: helperState.probe.installed,
      });
      if (!sameHostScope(scope, hostScopeRef.current)) return;
      if (!report.ok) {
        dispatchHelper({ type: "upgradeFailed", connectionKey, message: report.message, rollback: report.rollback });
        return;
      }
      dispatchHelper({ type: "upgradeSucceeded", connectionKey, message: report.message });
      setConnectionDetail(`Remote helper ${helperState.probe.installed ? "upgraded" : "installed"}; reconnecting for a fresh authoritative snapshot.`);
      setConnectionEpoch((value) => value + 1);
    } catch (error) {
      if (!sameHostScope(scope, hostScopeRef.current)) return;
      dispatchHelper({ type: "upgradeFailed", connectionKey, message: String(error), rollback: "notNeeded" });
    }
  };

  const connect = () => {
    dispatchHost({ type: "reset" });
    setActiveSessionId(undefined);
    setActiveWindowId(undefined);
    if (connectionMode === "local") {
      const profile: HostProfile = { id: "local", label: "Local", connection: { mode: "local" } };
      setConnection(profile.connection);
      setSelectedProfileId(profile.id);
      // An explicit Connect is also the user's retry control. The selected
      // profile may already have updated `connection`, so changing that state
      // alone is not guaranteed to reconstruct a stalled bridge.
      setConnectionEpoch((value) => value + 1);
      void invoke("save_host_profile", { profile });
      setStatus("Discovering local tmux…");
      return;
    }
    const target = sshTarget.trim();
    if (!target) return setStatus("Enter an SSH host or config alias.");
    const configPath = sshConfigPath.trim();
    const profileId = profileIdForSshConnection(profiles, target, configPath);
    const nextConnection: ConnectionSpec = { mode: "ssh", profileId, target, ...(configPath ? { configPath } : {}) };
    const profile: HostProfile = { id: profileId, label: target, connection: nextConnection };
    setConnection(nextConnection);
    setConnectionEpoch((value) => value + 1);
    setSelectedProfileId(profile.id);
    setProfiles((current) => [...current.filter((item) => item.id !== profile.id), profile]);
    void invoke("save_host_profile", { profile }).catch((error) => setStatus(String(error)));
    setStatus(`Connecting to ${target}…`);
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
        onRenameAgent={(agent) => setTextPrompt({
          title: "Rename agent",
          label: "Agent name",
          initialValue: agent.displayName,
          submit: (name) => { setTextPrompt(undefined); agentWorkflow.rename(agent, name); },
        })}
        onResumeAgent={agentWorkflow.resume}
        onReviewHooks={agentWorkflow.reviewHooks}
        onSelectAgent={selectAgentRow}
        onSelectWorkspace={selectSession}
        onSortMode={(mode) => updateShell({ agentSort: mode })}
        onWorkspaceCommand={(session, commandId) => void runCommand(commandId, { kind: "session", id: session.id })}
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
          onClose={closeCombinedTab}
          onMove={moveCombinedTab}
          onNewTerminal={() => void runCommand("window.new")}
          onPin={(tab) => pinOpenTab(tab.id)}
          onRenameTerminal={(tab) => void runCommand("window.rename", { kind: "terminalTab", id: tab.id })}
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
          {selectedAppTab ? <Suspense fallback={<p className="quiet-empty">Loading editor…</p>}>{selectedAppTab.kind === "gitDiff" ? <GitDiffSurface
            activeRoot={workspaceFiles.root}
            canWrite={hostState.canMutate}
            client={gitClient}
            onMessage={setStatus}
            onStatus={(next) => { if (workspaceFiles.root?.path === next.repository.worktreeRoot) workspaceGit.accept(next); }}
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
            onMeasurements={onMeasurements}
            grid={grid}
            handleInput={handleInput}
            hub={hub}
            mountedPanes={mountedPanes}
            paneAttention={agentRuntime.rollups.byPane}
            panes={panes}
            performAction={performAction}
            setStatus={setStatus}
            snapshot={snapshot}
            surfaceRef={surfaceRef}
            terminalTransferClient={terminalTransferClient}
            terminalTransferRegistry={terminalTransferRegistry}
            terminalTransferScope={terminalTransferScope}
          />}
        </div>
      </section>
      {panelOpen && <RightPanel
        files={<ExplorerTree
          disabled={!hostState.canMutate}
          error={workspaceFiles.error}
          expanded={workspaceFiles.expanded}
          ignoredPaths={ignoredPaths}
          listings={workspaceFiles.listings}
          loading={workspaceFiles.loading}
          requestedReads={workspaceFiles.requestedReads}
          onCancelTransfer={async (id) => { if (fileScope) await fileClient.cancelTransfer(fileScope, id); }}
          onDownload={async (intent) => { if (workspaceFiles.root) await startDownloadFlow(intent, workspaceFiles.root); }}
          onLoadMore={workspaceFiles.loadMore}
          onMutate={mutateFile}
          onOpen={openExplorerEntry}
          onRefresh={workspaceFiles.refresh}
          onToggle={workspaceFiles.toggleDirectory}
          root={workspaceFiles.root}
          scopeIdentity={fileScope ? keyForScope(fileScope) : "disconnected"}
          transfers={workspaceFiles.transfers}
        />}
        git={<GitSidebar
          client={gitClient}
          disabled={!hostState.canMutate}
          error={workspaceGit.error}
          loading={workspaceGit.loading}
          onMessage={setStatus}
          onOpenDiff={(entry, target) => {
            if (!activeSession || !hostState.serverIdentity || !workspaceFiles.root || !workspaceGit.status) return;
            setAppState((current) => openGitDiffTab(
              current,
              currentHostProfileId,
              hostState.serverIdentity!,
              activeSession,
              entry,
              target,
              workspaceGit.status!,
              workspaceFiles.root!,
            ));
          }}
          onRefresh={() => void workspaceGit.refresh()}
          onStatus={workspaceGit.accept}
          root={workspaceFiles.root}
          scope={fileScope}
          status={workspaceGit.status}
        />}
        onSurface={(surface) => void runCommand(surface === "files" ? "view.showFiles" : "view.showGit")}
        surface={appState.shell.panelSurface}
      />}
    </div>

    {/* The visible half of the status channel. Fixed-position, so it never
        takes a pixel from the terminal surface the client size is measured
        from; the full text is also in the live region below. */}
    {notice && <div className={notice.severity === "problem" ? "toast toast-problem" : "toast"} role={notice.severity === "problem" ? "alert" : "status"}>
      {/* A problem is usually a host rejection arriving verbatim — the raw
          `file_mutation_rejected: …` of M10-E059. It gets the summary-plus-
          disclosure treatment; ordinary progress is already a sentence and is
          left alone. The live region below still carries the full text. */}
      {notice.severity === "problem"
        ? <SurfaceError className="toast-body" detail={notice.message} role="none" />
        : <span>{notice.message}</span>}
      {/* Only on the notice this exact download raised: matching the message
          means a later status replaces the buttons along with the text, so
          they can never end up offering a file the toast is not about. */}
      {/* One row, not three: `.toast` is a grid, so bare buttons each take a
          line of their own. `.toast > div` is the existing row treatment the
          sibling toasts below already use. */}
      <div>
        {completedDownload?.noticeId === notice.id && <DownloadActions destination={completedDownload.destination} onResult={(error) => { if (error) setStatus(error); }} />}
        <button aria-label="Dismiss" onClick={() => { setNotice(undefined); setCompletedDownload(undefined); }} type="button">Dismiss</button>
      </div>
    </div>}
    {profileRecovery && <div className="toast" role="alert"><strong>Saved host profiles were recovered</strong><span>{profileRecovery.error} The original was preserved at {profileRecovery.preservedPath}.</span><button onClick={() => setProfileResetConfirmation(true)} type="button">Confirm recovered defaults…</button></div>}
    {appStateRecovery && <div className="toast" role="alert"><strong>Saved shell state is write-frozen</strong><span>{appStateRecovery}</span><button onClick={() => setAppStateResetConfirmation(true)} type="button">Reset saved shell state…</button></div>}
    {pendingAppRecovery && <div className="toast" role="status"><strong>App tabs found from the replaced tmux server</strong><span>{pendingAppRecovery.count} tab{pendingAppRecovery.count === 1 ? "" : "s"} can be rebound by unique workspace name. Terminal and pane identities are never reused.</span><div><button onClick={() => {
      if (!sameHostScope(pendingAppRecovery.scope, hostScopeRef.current)) return setPendingAppRecovery(undefined);
      setAppState((current) => recoverAppTabsFromPreviousServer(current, pendingAppRecovery.hostProfileId, pendingAppRecovery.previousServerIdentity, pendingAppRecovery.currentServerIdentity, snapshot.sessions));
      setPendingAppRecovery(undefined);
    }} type="button">Restore app tabs</button><button onClick={() => setAppRecoveryDiscardConfirmation(true)} type="button">Discard old tabs…</button></div></div>}

    <TerminalTransferHistory client={terminalTransferClient} onError={(error) => setStatus(String(error))} registry={terminalTransferRegistry} />
    {agentWorkflow.dialog}
    {agentHostSetup.dialog}
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
      appRecoveryDiscard={appRecoveryDiscardConfirmation ? pendingAppRecovery : undefined}
      appStateResetConfirmation={appStateResetConfirmation}
      commandContext={commandContext}
      confirmation={confirmation}
      helperState={helperState}
      hostDelete={hostDeleteConfirmation}
      onAppRecoveryDiscardCancel={() => setAppRecoveryDiscardConfirmation(false)}
      onAppRecoveryDiscardConfirm={() => {
        if (!pendingAppRecovery) return;
        setAppState((current) => discardServerAppState(current, pendingAppRecovery.hostProfileId, pendingAppRecovery.previousServerIdentity));
        setAppRecoveryDiscardConfirmation(false);
        setPendingAppRecovery(undefined);
      }}
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
