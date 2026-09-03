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
import { recordIncident } from "../diagnostics/incidents";
import type { TmuxAction } from "../features/tmux/actions";
import { TauriAgentClient } from "../features/agents/api";
import type { AgentRuntimeScope } from "../features/agents/types";
import { buildAgentRows, jumpTarget, unreadCount, type AgentListRow } from "../features/agents/agentsList";
import { useRecentIdleClock } from "../features/agents/useRecentIdleClock";
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
import { resetHostLatency, useHostLatency } from "../features/shell/hostLatency";
import {
  helperConnectionKey, helperOwnsHostSetupLane, helperUpgradeReducer, initialHelperUpgradeState,
} from "../features/shell/helperUpgrade";
import { sameHelperInstallConnection, sameHostConnection, type HostScopeToken } from "../features/shell/hostScope";
import { useShellCommands } from "../features/shell/useShellCommands";
import { effectiveRails } from "../features/shell/responsiveShell";
import { usePersistedAppState } from "../features/shell/usePersistedAppState";
import {
  clampedAgentsRatio, panelWidthForWindow, sidebarWidthForWindow,
  PANEL_MIN_WIDTH, SIDEBAR_MIN_WIDTH,
  type AppOwnedTab, type HostSetupDecision, type ShellState, type WorkspaceDefaults,
} from "../features/shell/types";
import {
  combineWorkspaceTabs,
  closeAppTab,
  findGitDiffTab,
  mountedAppTabIds,
  mountedTerminalPanes,
  openFileTab,
  openGitDiffTab,
  pinAppTab,
  retirePendingTab,
  selectableTabs,
  selectAppTab,
  setMarkdownViewMode,
  setWorkspaceDefaults,
  shouldSurfaceAuthoritativeTerminal,
  tabsToCloseOthers,
  tabsToCloseNonAgent,
  tabsToCloseRight,
  workspaceDefaultsFor,
  type CombinedTab,
  type AgentPresenceSnapshot,
  type PendingShellTab,
} from "../features/shell/model";
import { useContextMenusOpen } from "../ui/ContextMenu";
import { useFocusHistoryNavigation } from "./useFocusHistoryNavigation";
import { TabStrip, workspaceTabDomId, workspaceTabPanelDomId } from "../features/workspaces/TabStrip";
import {
  hostWorkspaceRows, mergeHostRows, pinnedOnlyMergedRows, type HostRowSource, type MergedWorkspaceRow,
} from "../features/workspaces/mergedWorkspaceRows";
import { WorkspaceSidebar, type SidebarHost } from "../features/workspaces/WorkspaceSidebar";
import { WorkspaceSwitcher } from "../features/workspaces/WorkspaceSwitcher";
import { inferHome } from "../features/workspaces/workspaceRows";
import { hostLetter } from "../features/shell/hostProfiles";
import type { CommandHost } from "../features/shell/useShellCommands";
import { denormalizeSnapshot } from "../state/connectionReducer";
import { hostLinkScope } from "../state/hostLinks";
import type { AgentAttentionRollup, AgentRecord } from "../features/agents/types";
import type { AgentAdapterDescriptor } from "../features/agents/types";
import type { ConnectionSpec, HostProfile, Pane, TmuxSnapshot } from "./types";
import { resolveTerminalDestination } from "./paneRouting";
import { useAppConnectionController } from "./useAppConnectionController";
import { usePerHostMemo } from "./usePerHostMemo";
import { useAppRecoveryController } from "./useAppRecoveryController";
import { useClientResize } from "./useClientResize";
import { useVisibleTerminalSession } from "./useVisibleTerminalSession";
import { commitScopedAppTabClose, reportAnnouncedPaneResult, useShellNavigation } from "./useShellNavigation";
import { useTmuxActionPerformer, type TmuxActionTarget } from "./useTmuxActionPerformer";
import { useWorkspaceCreate } from "./useWorkspaceCreate";
import { windowCellSize } from "../features/terminal/clientSize";
import { useWorkspaceDomainController } from "./useWorkspaceDomainController";
import { AppDialogLayer } from "./AppDialogLayer";
import { TerminalWorkspaceSurface } from "./TerminalWorkspaceSurface";
import { useAppAgentController } from "./useAppAgentController";
import { useAppShellChrome } from "./useAppShellChrome";
import { useAppHostSettingsActions } from "./useAppHostSettingsActions";
import { useRemoteHelperRecovery } from "./useMissingHelperRecovery";
import { useAppFileActions } from "./useAppFileActions";
import { AppNoticeLayer } from "./AppNoticeLayer";
import { AppRightPanel } from "./AppRightPanel";
import { useBulkTabClose } from "./useBulkTabClose";
import { useTerminalFileOpen } from "./useTerminalFileOpen";

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
 * When a keystroke took too long to *leave* the app.
 *
 * The input invoke blocks while the native send queue is full, so this is the
 * outbound half of the lag the echo probe measures: past this, the delay is
 * already on this side of the link and no amount of server-side latency
 * explains it.
 */
const SLOW_SEND_THRESHOLD_MS = 250;

/** A backed-up queue produces one line per episode, not one per keystroke. */
const SLOW_SEND_INTERVAL_MS = 10_000;

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

const NO_AGENTS: readonly AgentRecord[] = [];
const NO_ADAPTERS: readonly AgentAdapterDescriptor[] = [];
const NO_ATTENTION: ReadonlyMap<string, AgentAttentionRollup> = new Map();
const NO_SNAPSHOT: TmuxSnapshot = { sessions: [], windows: [], panes: [] };

/**
 * The scope of a host with no link: a saved host that is not shown, or one
 * whose link went while a menu over it stayed open. Nothing captured can
 * match it, so every action checked against it is refused.
 */
function unlinkedScope(profileId: string): HostScopeToken {
  return { hostProfileId: profileId, connectionKey: "", connectionEpoch: -1, generation: 0 };
}

/** What to call a host that has no saved profile to name it. */
function connectionLabel(connection: ConnectionSpec): string {
  return connection.mode === "local" ? "Local" : connection.target;
}

/** A row, agent or bell jump on another host, waiting for the facade to report that host. */
interface PendingHostSelection {
  profileId: string;
  sessionId: string;
  paneId?: string;
  /** Who asked, for the announcement once the pane is on screen. */
  source: string;
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

export function App() {
  // The sequence rides along with the text so that the same message twice —
  // a create refused for the same reason after its notice was dismissed — is
  // two notices, not one that the second attempt silently fails to re-show.
  const [statusState, setStatusState] = useState({ text: "Discovering local tmux…", sequence: 0 });
  const status = statusState.text;
  const setStatus = useCallback((text: string) => {
    setStatusState((current) => ({ text, sequence: current.sequence + 1 }));
  }, []);
  const {
    compactViewport, completedDownload, notice, setCompletedDownload, setNotice, windowWidth,
  } = useAppShellChrome(status, statusState.sequence);
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
  const platform = useMemo(() => currentPlatform(), []);
  const { appState, appStateRecovery, resetAppState, setAppState } = usePersistedAppState(setStatus, platform);
  // Read by things that run later than the render that scheduled them — the
  // workspace-create prompt is submitted long after the command that opened it,
  // and the defaults it applies must be the ones in force at that moment.
  const appStateRef = useRef(appState);
  appStateRef.current = appState;
  // Stable identity: the hooks that read it hold it in a dependency list, and a
  // fresh closure per render would rebuild them on every keystroke.
  const readDefaultMarkdownView = useCallback(() => appStateRef.current.shell.defaultMarkdownView, []);
  // One shared observation per repository, for the sidebar and every diff tab.
  const gitRepositories = useMemo(() => new GitRepositoryStore(gitClient), [gitClient]);
  // The connection controller reports helper-relevant lifecycle points; what
  // to do about them is decided further down this component, with the helper
  // reducer in hand. The indirection lets the two be defined in that order.
  const onHandshakeFailure = useRef<(connection: ConnectionSpec) => void>(() => undefined);
  const onConnectionStateChanged = useRef<NonNullable<
    Parameters<typeof useAppConnectionController>[0]["onConnectionStateChanged"]
  >>(() => undefined);
  const connectionController = useAppConnectionController({
    agentClient,
    fileClient,
    gitClient,
    terminalApplicationClipboardEnabled: appState.shell.terminalApplicationClipboard,
    onHandshakeFailure: (failed) => onHandshakeFailure.current(failed),
    onConnectionStateChanged: (changed, state) => onConnectionStateChanged.current(changed, state),
    setStatus,
  });
  const {
    activateHost, activeSessionId, activeWindowId, appFocused, clientHostProfileId, clientId, clientIdRef, connection,
    connectionDetail, connectionEpoch, connectionMode, currentHostProfileId,
    currentHostScope, dispatchHost, echoLagProbe, hostScopeRef, hostState, hub, inputLatencyReporter, linkFor, links,
    optimisticWindow, profileRecovery,
    profiles, selectedProfileId, setActiveSessionId, setActiveWindowId,
    setConnection, setConnectionDetail, setConnectionEpoch, setConnectionMode,
    setProfileRecovery, setProfiles, setSelectedProfileId, setSshConfigPath, setSshTarget,
    snapshot, snapshotRef, sshConfigPath, sshTarget, terminalEpoch, windows,
  } = connectionController;
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
  const controllers = useRef(new Map<string, TerminalPaneController>());
  const lastSlowSendAt = useRef(new Map<string, number>());
  const shortcuts = appState.commands.shortcutOverrides as ShortcutOverrides;
  const currentHelperConnectionKey = helperConnectionKey(connection);
  const terminalTransferClient = useMemo(() => new TauriTerminalTransferClient(), []);
  const terminalTransferRegistry = useTerminalTransferRegistry();
  const latency = useHostLatency();
  useEffect(() => dispatchHelper({ type: "reset" }), [connectionEpoch, currentHelperConnectionKey]);
  const remoteHelperRecovery = useRemoteHelperRecovery({
    connection, connectionEpoch, dispatchHelper, setConnectionDetail,
  });
  onHandshakeFailure.current = remoteHelperRecovery.onHandshakeFailure;
  onConnectionStateChanged.current = remoteHelperRecovery.onConnectionStateChanged;
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
    activateHost,
    clearActiveSelection: () => {
      setActiveSessionId(undefined);
      setActiveWindowId(undefined);
    },
    connection,
    connectionMode,
    currentScope: currentHostScope,
    dispatchHelper,
    helperState,
    probeHelper: remoteHelperRecovery.probeManually,
    profiles,
    resetHost: () => dispatchHost({ type: "reset" }),
    scopeIsCurrent: (scope) => sameHelperInstallConnection(scope, hostScopeRef.current),
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
  // Every host with a bridge, the active one included. A host that leaves the
  // list loses its agent records.
  const shownHostIds = useMemo(() => links.map((link) => link.profileId), [links]);
  // Every other shown host with a live, mutable client: its agents are
  // requested under its own scope and routed to its own slice. The scope
  // carries the host's topology windows, the same way the active host's
  // scope does, so its snapshot can prove an agent's window is gone.
  const peerScopes = useMemo<AgentRuntimeScope[]>(() => links.flatMap((link) => {
    const { serverIdentity, canMutate, generation, windows: topologyWindows } = link.hostState;
    if (link.profileId === currentHostProfileId || !link.clientId || !serverIdentity || !canMutate) return [];
    return [{
      scope: {
        clientId: link.clientId,
        hostProfileId: link.profileId,
        serverIdentity,
        topologyGeneration: generation,
        connectionEpoch: link.terminalEpoch,
      },
      topologyWindowIds: Object.keys(topologyWindows),
    }];
  }), [currentHostProfileId, links]);
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
    // Helper compatibility owns the host-level consent lane while it is
    // unresolved. Runtime observation stays live; only the separate one-time
    // agent setup question waits, so two dialogs can never stack.
    hostSetupAllowed: !helperOwnsHostSetupLane(helperState),
    hostLabel,
    peerScopes, shownHostIds,
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
  // Letters only once a second host is shown beside the first: one host has
  // nothing to be told apart from.
  const showLetters = links.length >= 2;
  /**
   * Each host's world as its rows read it, memoized on that host's state
   * alone: a snapshot arriving on one host must not rebuild another host's
   * rows. The active host's snapshot is the facade's own object.
   */
  const hostWorlds = usePerHostMemo(links.map((link) => ({
    key: link.profileId,
    deps: [link.hostState],
    build: () => {
      const world = link.profileId === currentHostProfileId ? snapshot : denormalizeSnapshot(link.hostState);
      return { snapshot: world, home: inferHome(world.panes.map((pane) => pane.currentPath)) };
    },
  })));
  // `hostLinkScope` is a fresh object every render; the rows that carry it
  // must only be rebuilt when what it says changes, or every status line and
  // latency sample would re-sort every workspace and agent row.
  const hostScopes = usePerHostMemo(links.map((link) => {
    const scope = hostLinkScope(link);
    return {
      key: link.profileId,
      deps: [scope.hostProfileId, scope.connectionKey, scope.connectionEpoch, scope.serverIdentity, scope.generation],
      build: () => scope,
    };
  }));
  const hostSources = usePerHostMemo(links.map((link) => {
    const active = link.profileId === currentHostProfileId;
    const profile = profiles.find((item) => item.id === link.profileId);
    const label = profile?.label ?? connectionLabel(link.connection);
    const letter = hostLetter(profile ?? { id: link.profileId, label });
    const world = hostWorlds.get(link.profileId)!;
    const scope = hostScopes.get(link.profileId)!;
    // The active host's agents are the runtime's own projection, which it
    // keeps whether or not the host has a live agent scope; a peer's come
    // from its slice, and a peer with no scope yet has none.
    const agents = active ? agentRuntime : agentRuntime.byHost.get(link.profileId);
    const { phase, canMutate } = link.hostState;
    const transport = link.connection.mode;
    const linkSessionId = active ? activeSessionId : undefined;
    const activeBranch = active ? workspaceGit.status?.repository.headName : undefined;
    return {
      key: link.profileId,
      deps: [letter, label, phase, canMutate, transport, scope, world, linkSessionId, agents?.agents, agents?.adapters, agents?.rollups.byWorkspace, activeBranch],
      build: (): HostRowSource => ({
        hostProfileId: link.profileId,
        letter,
        label,
        phase,
        canMutate,
        transport,
        scope,
        snapshot: world.snapshot,
        activeSessionId: linkSessionId,
        agents: agents?.agents ?? NO_AGENTS,
        adapters: agents?.adapters ?? NO_ADAPTERS,
        attentionByWorkspace: agents?.rollups.byWorkspace ?? NO_ATTENTION,
        activeBranch,
        home: world.home,
      }),
    };
  }));
  const hostRows = usePerHostMemo(links.map((link) => {
    const source = hostSources.get(link.profileId)!;
    return { key: link.profileId, deps: [source, showLetters], build: () => hostWorkspaceRows(source, showLetters) };
  }));
  /**
   * Every saved host in profile order, for the host menu; the ones with a
   * link carry that link's phase and scope. The host on screen is always
   * among them, saved or not — the render between Connect and its save, or
   * the moment before the profiles have loaded.
   */
  const sidebarHosts = useMemo<SidebarHost[]>(() => {
    const byProfileId = new Map(links.map((link) => [link.profileId, link]));
    const hosts = profiles.map((profile): SidebarHost => {
      const link = byProfileId.get(profile.id);
      const active = profile.id === currentHostProfileId;
      return {
        profileId: profile.id,
        letter: hostLetter(profile),
        label: profile.label,
        transport: profile.connection.mode,
        phase: link?.hostState.phase ?? "disconnected",
        canMutate: link?.hostState.canMutate ?? false,
        scope: hostScopes.get(profile.id) ?? unlinkedScope(profile.id),
        active,
        shown: active || Boolean(profile.shown),
        latencyMs: active ? latency?.milliseconds : undefined,
      };
    });
    if (!hosts.some((host) => host.active)) {
      const label = connectionLabel(connection);
      hosts.push({
        profileId: currentHostProfileId,
        letter: hostLetter({ id: currentHostProfileId, label }),
        label,
        transport: connection.mode,
        phase: hostState.phase,
        canMutate: hostState.canMutate,
        scope: hostScopes.get(currentHostProfileId) ?? unlinkedScope(currentHostProfileId),
        active: true,
        shown: true,
        latencyMs: latency?.milliseconds,
      });
    }
    return hosts;
  }, [connection, currentHostProfileId, hostScopes, hostState.canMutate, hostState.phase, latency?.milliseconds, links, profiles]);
  // Every workspace on every shown host, pinned first. ⌘P reads this whole;
  // the sidebar, ⌘1–9 and the agents list read the narrowed version below.
  const switcherRows = useMemo(() => mergeHostRows([...hostRows.values()]), [hostRows]);
  const sidebarRows = useMemo(
    () => appState.shell.pinnedOnly
      ? pinnedOnlyMergedRows(switcherRows, { hostProfileId: currentHostProfileId, sessionId: activeSessionId })
      : switcherRows,
    [activeSessionId, appState.shell.pinnedOnly, currentHostProfileId, switcherRows],
  );
  // Every host's agents, in host order: the list, the bell and its count
  // span every shown host, because "who needs me" is not a question about
  // the machine that happens to be on screen.
  const allAgents = useMemo(() => [...hostSources.values()].flatMap((source) => source.agents), [hostSources]);
  const recentIdleClock = useRecentIdleClock(allAgents, appState.shell.agentSort === "status");
  const agentRows = useMemo(() => {
    const orderByRowKey = new Map(sidebarRows.map((row, index) => [row.key, index]));
    // Every window on each server, not just the workspace on screen: the
    // agents list spans workspaces, so it needs the pins of tabs whose strip
    // is not currently drawn.
    const hosts = new Map([...hostSources.values()].map((source) => [source.hostProfileId, {
      label: source.label,
      letter: showLetters ? source.letter : "",
      windowIndexById: new Map(source.snapshot.windows.map((item) => [item.id, item.index])),
      paneIds: new Set(source.snapshot.panes.map((pane) => pane.id)),
      pinnedWindowIds: new Set(source.snapshot.windows.filter((item) => item.pinned).map((item) => item.id)),
      pinnedSessionIds: new Set(source.snapshot.sessions.filter((item) => item.pinned).map((item) => item.id)),
    }]));
    return buildAgentRows(
      allAgents,
      (record) => {
        const host = hosts.get(record.hostProfileId);
        return {
          workspaceOrder: orderByRowKey.get(`${record.hostProfileId}\0${record.sessionId}`) ?? Number.MAX_SAFE_INTEGER,
          workspaceName: record.sessionName || "unknown workspace",
          hostLabel: host?.label ?? record.hostProfileId,
          hostLetter: host?.letter ?? "",
          tabIndex: host?.windowIndexById.get(record.windowId),
          workspacePinned: host?.pinnedSessionIds.has(record.sessionId) ?? false,
          tabPinned: host?.pinnedWindowIds.has(record.windowId) ?? false,
        };
      },
      (record) => Boolean(record.paneId) && Boolean(hosts.get(record.hostProfileId)?.paneIds.has(record.paneId)),
      appState.shell.agentSort,
      Date.now(),
    );
  }, [allAgents, appState.shell.agentSort, hostSources, recentIdleClock, showLetters, sidebarRows]);
  // What the agents section lists, which under the filter is not everything
  // the shell knows about. Only the section is narrowed: the bell, its count
  // and ⌘⇧U keep reading the whole list, because "who needs me" is a question
  // about every agent on every host and a filter over the sidebar is not an
  // instruction to stop counting the rest.
  const visibleAgentRows = useMemo(() => {
    if (!appState.shell.pinnedOnly) return agentRows;
    const visible = new Set(sidebarRows.map((row) => row.key));
    return agentRows.filter((row) => visible.has(`${row.agent.hostProfileId}\0${row.agent.sessionId}`));
  }, [agentRows, appState.shell.pinnedOnly, sidebarRows]);
  const unread = useMemo(() => unreadCount(agentRows), [agentRows]);
  // The badge counts every waiting agent; the bell can only reach routable
  // ones, so it is disabled on exactly the rows `agents.jumpUnread` would find.
  const canJump = useMemo(() => Boolean(jumpTarget(agentRows)), [agentRows]);

  // Only in its own workspace's strip: a create-session placeholder has no
  // session until its ack names one, and drawing it anywhere before that would
  // put it in the workspace being navigated away from.
  const pendingTabHere = pendingTab && pendingTab.sessionId === activeSessionId ? pendingTab : undefined;
  // Where the placeholder actually retires, from the same window list the strip
  // draws it against. Only for the workspace on screen: another workspace's
  // window list is not in `windows`, so retiring its placeholder here would be
  // retiring it on no evidence at all. The identity check keeps a create that
  // started in the meantime — the second of two quick clicks — from being
  // retired by the first one's snapshot.
  useEffect(() => {
    if (!pendingTabHere) return;
    setPendingTab((current) => (current === pendingTabHere ? retirePendingTab(current, windows) : current));
  }, [pendingTabHere, windows]);
  const acceptedAgentTopology = agentRuntime.state.authoritative
    && agentRuntime.state.hostProfileId === currentHostProfileId
    && agentRuntime.state.serverIdentity === hostState.serverIdentity
    && agentRuntime.state.connectionEpoch === agentScope?.connectionEpoch
    ? agentRuntime.topologyAuthority
    : undefined;
  const currentAgentTopology = agentScope ? {
    hostProfileId: agentScope.hostProfileId,
    serverIdentity: agentScope.serverIdentity,
    connectionEpoch: agentScope.connectionEpoch,
    topologyGeneration: hostState.generation,
  } : undefined;
  const hasUnmappedAgents = agentRuntime.agents.some((agent) => !agent.windowId || !agent.sessionId || !agent.paneId);
  const agentPresence: AgentPresenceSnapshot = {
    accepted: acceptedAgentTopology,
    current: currentAgentTopology,
    byWindow: agentRuntime.rollups.byWindow,
    hasUnmappedAgents,
  };
  const agentPresenceRef = useRef(agentPresence);
  agentPresenceRef.current = agentPresence;
  const combinedTabs = useMemo(
    // The same authority the commit-time recheck reads, `hasUnmappedAgents`
    // included: the strip and the recheck must not disagree about which
    // windows are provably empty.
    () => combineWorkspaceTabs(
      windows, workspaceAppTabs, agentRuntime.rollups.byWindow, pendingTabHere, {
        accepted: acceptedAgentTopology,
        current: currentAgentTopology,
        hasUnmappedAgents,
      },
    ),
    [acceptedAgentTopology, agentRuntime.rollups.byWindow, currentAgentTopology, hasUnmappedAgents, pendingTabHere, windows, workspaceAppTabs],
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
  const revealTerminalUnderAppTab = useCallback((sessionId: string, windowId: string | undefined) => {
    notificationActivation.clearNotificationFocusGuard();
    shellNavigation.revealLocalTerminal(sessionId, windowId, () => setNavigationAppTab(sessionId, undefined));
  }, [notificationActivation, setNavigationAppTab, shellNavigation]);
  const focusNavigation = useFocusHistoryNavigation({
    activeSessionId,
    hostProfileId: currentHostProfileId,
    activeWindowId,
    appTabs: appState.appTabs,
    revealTerminal: revealTerminalUnderAppTab,
    selectAppTab: shellNavigation.selectAppTab,
    selectSession,
    selectWindow,
    selectedAppTabId: selectedAppTab?.id,
    sessions: snapshot.sessions,
    setStatus,
    windows: snapshot.windows,
  });
  /** Whether a captured scope is still the live connection of the host it names, active or shown beside it. */
  const scopeIsLive = useCallback((scope: HostScopeToken) => {
    const link = linkFor(scope.hostProfileId);
    return link !== undefined && sameHostConnection(scope, hostLinkScope(link));
  }, [linkFor]);
  /**
   * How an action reaches a host shown beside the active one; nothing for
   * the host on screen, whose path is the performer's default. The scope is
   * read live from the link, so a connection that moves on under the
   * request — or a host that stops being shown — discards the result.
   */
  const actionTargetFor = useCallback((hostProfileId: string): TmuxActionTarget | undefined => {
    if (hostProfileId === hostScopeRef.current.hostProfileId) return undefined;
    const link = linkFor(hostProfileId);
    return {
      clientId: link?.clientId,
      canMutate: link?.hostState.canMutate ?? false,
      scopeRef: {
        get current() {
          const now = linkFor(hostProfileId);
          return now ? hostLinkScope(now) : unlinkedScope(hostProfileId);
        },
      },
    };
  }, [hostScopeRef, linkFor]);
  // Both pins are host state now, so both take the action pipeline: the pin
  // outlives this app, and every client of the same tmux server sees it. The
  // scope check is the same one every row action makes — a menu can outlive the
  // connection it was opened over.
  //
  // Deliberately without an `execution`, like the workspace reorder these now
  // resemble: navigation execution rethrows a refusal for its caller to handle,
  // and a pin has no navigation to abandon — a host that says no is a status
  // line and an incident, not an exception nobody is waiting for. The pin state
  // is read from the same live row or tab the menu label was drawn from, so the
  // action asks for the opposite of what the user was just shown.
  //
  // A row on a host shown beside the active one carries that host's scope,
  // and its pin goes to that host's client — the row is where the pin lives.
  const toggleWorkspacePin = useCallback((row: MergedWorkspaceRow) => {
    if (!scopeIsLive(row.scope)) return;
    void performAction(
      { kind: "setPinned", sessionId: row.session.id, pinned: !row.session.pinned },
      undefined, undefined, actionTargetFor(row.hostProfileId),
    );
  }, [actionTargetFor, performAction, scopeIsLive]);
  const toggleTabPin = useCallback((tab: Extract<CombinedTab, { kind: "terminal" }>, scope: HostScopeToken) => {
    if (!activeSessionId || !sameHostConnection(scope, hostScopeRef.current)) return;
    void performAction({ kind: "setPinned", sessionId: activeSessionId, windowId: tab.id, pinned: !tab.pinned });
  }, [activeSessionId, hostScopeRef, performAction]);
  // From the agents list the tab is named by the agent's own route, which
  // spans workspaces — no dependence on the session on screen.
  const toggleAgentTabPin = useCallback((row: AgentListRow, scope: HostScopeToken) => {
    if (!scopeIsLive(scope)) return;
    void performAction(
      { kind: "setPinned", sessionId: row.agent.sessionId, windowId: row.agent.windowId, pinned: !row.location.tabPinned },
      undefined, undefined, actionTargetFor(row.agent.hostProfileId),
    );
  }, [actionTargetFor, performAction, scopeIsLive]);
  /**
   * The host a targeted command runs on, by the scope its row captured. The
   * active host answers with the facade's own state and action path; a peer
   * answers with its link's snapshot and an action path aimed at its client.
   * A scope that is no longer any shown host's live connection is nothing.
   */
  const commandHostForScope = useCallback((scope: HostScopeToken): CommandHost | undefined => {
    if (!scopeIsLive(scope)) return undefined;
    const target = actionTargetFor(scope.hostProfileId);
    if (!target) {
      return {
        scope: hostScopeRef.current, snapshot: snapshotRef.current, serverIdentity: hostScopeRef.current.serverIdentity,
        performAction, isScopeCurrent: scopeIsLive,
      };
    }
    const linkScope = target.scopeRef.current;
    return {
      scope: linkScope,
      snapshot: hostWorlds.get(scope.hostProfileId)?.snapshot ?? NO_SNAPSHOT,
      serverIdentity: linkScope.serverIdentity,
      performAction: (action, precondition) => performAction(action, precondition, undefined, target),
      isScopeCurrent: scopeIsLive,
    };
  }, [actionTargetFor, hostScopeRef, hostWorlds, performAction, scopeIsLive, snapshotRef]);

  /**
   * A row on another host: the host becomes the one on screen first, and the
   * row's session — and pane, for an agent — is selected once the facade
   * reports that host. Bell jumps, ⌘1–9, ⌘P and agent rows all arrive here.
   */
  const [pendingHostSelection, setPendingHostSelection] = useState<PendingHostSelection>();
  const selectOnHost = useCallback((selection: PendingHostSelection) => {
    setPendingHostSelection(selection);
    activateHost(selection.profileId);
  }, [activateHost]);
  const selectWorkspaceRow = useCallback((row: MergedWorkspaceRow) => {
    if (row.hostProfileId === hostScopeRef.current.hostProfileId) return selectSession(row.session.id);
    selectOnHost({ profileId: row.hostProfileId, sessionId: row.session.id, source: `Workspace ${row.session.name}` });
  }, [hostScopeRef, selectOnHost, selectSession]);
  useEffect(() => {
    if (!pendingHostSelection || pendingHostSelection.profileId !== currentHostProfileId) return;
    setPendingHostSelection(undefined);
    const { paneId, sessionId, source } = pendingHostSelection;
    const destination = paneId ? resolveTerminalDestination(snapshot.panes, paneId) : undefined;
    if (destination?.kind === "target") {
      void surfacePaneDestination(destination.pane, source).then((result) => reportAnnouncedPaneResult(result, setStatus));
      return;
    }
    if (destination) setStatus(`${source}'s pane is no longer available: ${destination.reason}; opening its workspace.`);
    selectSession(sessionId);
  }, [currentHostProfileId, pendingHostSelection, selectSession, snapshot.panes, surfacePaneDestination]);

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

  // From the list the row comes with its host's scope; the bell jump has
  // none, and asks for whichever agent is loudest on whichever host.
  const selectAgentRow = useCallback((row: AgentListRow, scope?: HostScopeToken) => {
    notificationActivation.clearNotificationFocusGuard();
    if (scope && !scopeIsLive(scope)) return;
    if (!row.agent.paneId) return setStatus(`Agent ${row.agent.displayName} has no exact pane match; navigation is unavailable.`);
    const source = `Agent ${row.agent.displayName}`;
    if (row.agent.hostProfileId !== hostScopeRef.current.hostProfileId) {
      return selectOnHost({ profileId: row.agent.hostProfileId, sessionId: row.agent.sessionId, paneId: row.agent.paneId, source });
    }
    const destination = resolveTerminalDestination(snapshot.panes, row.agent.paneId);
    if (destination.kind === "unavailable") return setStatus(`Agent destination ${row.agent.displayName} is no longer available: ${destination.reason}.`);
    void surfacePaneDestination(destination.pane, source)
      .then((result) => reportAnnouncedPaneResult(result, setStatus));
  }, [hostScopeRef, notificationActivation, scopeIsLive, selectOnHost, snapshot.panes, surfacePaneDestination]);

  /**
   * The picked host's mark and visibility, saved as typed. Saving does not
   * move the last-profile pointer, and the controller derives its link set
   * from `profiles`, so a host checked here gets its bridge — and one
   * unchecked loses it — without anything else being told.
   */
  const saveProfileFields = useCallback((profileId: string, patch: Pick<HostProfile, "letter" | "shown">) => {
    const profile = profiles.find((item) => item.id === profileId);
    if (!profile) return;
    const { letter, ...rest } = { ...profile, ...patch };
    const next: HostProfile = letter ? { ...rest, letter } : rest;
    setProfiles((current) => current.map((item) => (item.id === profileId ? next : item)));
    void invoke("save_host_profile", { profile: next }).catch((error) => setStatus(String(error)));
  }, [profiles, setProfiles]);
  const toggleHostShown = useCallback((profileId: string) => {
    const profile = profiles.find((item) => item.id === profileId);
    // The host on screen is always shown; the menu disables its item.
    if (!profile || profileId === currentHostProfileId) return;
    saveProfileFields(profileId, { letter: profile.letter, shown: !profile.shown });
  }, [currentHostProfileId, profiles, saveProfileFields]);

  // What the Explorer, Git and the agents list currently offer for the row the
  // user last pointed at — the palette's only way to name a row.
  const rowCommands = useRowCommands();
  /**
   * The one commit path for closing an app tab, shared by the single close and
   * by the bulk closes. Neither reaches it through `runCommand`: that route
   * flushes every open editor first, which for a set of tabs would replay the
   * same flush once per tab.
   */
  const closeWorkspaceAppTab = (tab: AppOwnedTab, scope: HostScopeToken, mode: "single" | "bulk" = "single") => {
    const commit = () => setAppState((current) => closeAppTab(current, currentHostProfileId, tab.id));
    // Closing the document on screen goes back to what was showing before
    // it — the terminal it was opened from, usually — rather than to whatever
    // terminal the workspace happens to have active. With no history to go
    // back to, a neighbouring document in the same workspace is next, and
    // only then the workspace's terminal. A bulk close skips both: its
    // neighbours are about to go too, and the terminal is where it ends up.
    if (mode === "single" && selectedAppTabRef.current?.id === tab.id && sameHostConnection(scope, hostScopeRef.current)) {
      if (focusNavigation.navigateBackFromClosing(tab.id)) return commit();
      const neighbours = workspaceAppTabs.filter((item) => item.id !== tab.id);
      const neighbour = neighbours.filter((item) => item.order < tab.order).at(-1) ?? neighbours[0];
      if (neighbour) {
        shellNavigation.selectAppTab(tab.sessionId, activeWindowId, neighbour.id);
        return commit();
      }
    }
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
  const bulkCloseInFlight = useRef(false);
  const closeTabSet = useBulkTabClose({
    agentPresenceRef,
    closeAppTab: (tab, scope) => closeWorkspaceAppTab(tab, scope, "bulk"),
    hostScopeRef,
    performAction,
    setStatus,
    snapshotRef,
    workspaceAppTabs,
  });
  const openTerminalFilePath = useTerminalFileOpen({
    clientIdRef,
    defaultMarkdownView: readDefaultMarkdownView,
    fileClient,
    fileScope,
    hostScopeRef,
    selectLocalAppTab: shellNavigation.selectLocalAppTab,
    setAppState,
    setStatus,
    snapshotRef,
  });
  const createWorkspace = useWorkspaceCreate({
    appStateRef,
    clientIdRef,
    createSession: shellNavigation.createSession,
    currentHostProfileId,
    hostScopeRef,
    sendInput,
    setStatus,
  });
  const { commandContext, runCommand } = useShellCommands({
    activePane, activeSession, activeWindow, appState, canMutate: hostState.canMutate,

    closeAppTab: closeWorkspaceAppTab,
    combinedTabs, controllers, currentHostProfileId, deletableHostProfile: deletableProfile,
    focusDirection, hostScope: currentHostScope,
    hostForScope: commandHostForScope,
    isHostScopeCurrent: (scope) => sameHostConnection(scope, hostScopeRef.current),
    jumpToUnreadAgent: () => {
      const target = jumpTarget(agentRows);
      if (!target) return setStatus("No agent is waiting on you.");
      selectAgentRow(target);
    },
    performAction, requestHostProfileDelete: setHostDeleteConfirmation, rowCommands, selectedAppTab,
    createSession: createWorkspace,
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
      // Pending create placeholders are visible feedback, not shortcut
      // targets. The numbers drawn in the strip use this same real-tab order.
      const tab = selectableTabs(combinedTabs)[index];
      if (tab) selectCombinedTab(tab);
    },
    selectWorkspaceByIndex: (index) => {
      const row = sidebarRows[index];
      if (row) selectWorkspaceRow(row);
    },
    serverIdentity: hostState.serverIdentity, setAppState, setConfirmation,
    setPaletteOpen, setSettingsOpen, setShortcutEditorOpen, setStatus, setTextPrompt,
    setWorkspaceSwitcherOpen, snapshot,
    stepFocusHistory: focusNavigation.step,
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
      const command = commandForKeyboardEvent(event, platform, shortcuts);
      if (!command || !globalShortcutAllowed(event, modalOpen, command.id)) return;
      if (!commandAvailable(command, commandContext)) return;
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
    const sentAt = performance.now();
    const request = input.kind === "text" ? sendInput(clientId, paneId, input.data) : sendBinaryInput(clientId, paneId, input.data);
    // How long the keystroke took to *leave*. The native side blocks this
    // invoke while its input queue is full, so a slow one says the stall is on
    // the way out of the app — which a silent link cannot otherwise distinguish
    // from a keystroke that left promptly and died server-side.
    void request.then(() => {
      const outboundMs = performance.now() - sentAt;
      // The same span the outlier below reports, kept for every send this time:
      // the journal's `send` segment is what the end-to-end histogram has to be
      // decomposed against. A rejected send left nothing to measure. This site
      // cannot tell a typed key from xterm's own reply to a program query — the
      // probe's key gate is what knows that — so every dispatched batch is
      // sampled, and the segment reads as "cost of leaving the app".
      inputLatencyReporter.sample("send", outboundMs);
      if (outboundMs <= SLOW_SEND_THRESHOLD_MS) return;
      const previous = lastSlowSendAt.current.get(paneId);
      if (previous !== undefined && sentAt - previous < SLOW_SEND_INTERVAL_MS) return;
      lastSlowSendAt.current.set(paneId, sentAt);
      recordIncident("input.sendSlow", { paneId, ms: Math.round(outboundMs) });
    }).catch((error) => { if (clientIdRef.current === clientId) setStatus(String(error)); });
  }, [clientId, echoLagProbe, hostState.canMutate, inputLatencyReporter]);

  const handleKeyActivity = useCallback((paneId: string) => {
    echoLagProbe.noteKey(paneId);
  }, [echoLagProbe]);

  /** The render half of the same measurement — see `TerminalPane`'s sampler. */
  const handlePaintSample = useCallback((_paneId: string, ms: number) => {
    inputLatencyReporter.sample("paint", ms);
  }, [inputLatencyReporter]);

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
   * Runs a settled bulk close immediately.
   *
   * There is no preflight dialog. A close asked for from a tab strip is a
   * direct manipulation of the thing the person is pointing at, and a modal
   * between the click and the result made the four-tab case a two-step. What
   * replaces it is a receipt: the hook flushes dirty editors before it destroys
   * anything, refuses the whole set if a save fails, still protects terminals
   * holding agents, and reports afterwards.
   */
  const bulkCloseTabs = (tabs: CombinedTab[], scope: HostScopeToken, protectAgents = false) => {
    // One set at a time. The dialog used to serialize these by existing; with a
    // toolbar button in its place a second click lands while the first close is
    // still walking the set, and the second run reads the same pre-close
    // snapshot — dispatching closes for windows that are already gone and
    // ending in a sticky "N could not be closed" for an operation that worked.
    if (tabs.length === 0 || bulkCloseInFlight.current) return;
    bulkCloseInFlight.current = true;
    void closeTabSet(tabs, scope, protectAgents).finally(() => { bulkCloseInFlight.current = false; });
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
        current, currentHostProfileId, serverIdentity, session, entry.path, kind, root,
        { ...options, viewMode: current.shell.defaultMarkdownView },
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

  /** What to call the host in Settings; the profile’s own label wherever there is one. */
  const currentHostLabel = profiles.find((profile) => profile.id === currentHostProfileId)?.label
    ?? (currentHostProfileId === "local" ? "Local" : currentHostProfileId);

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
      canJump={canJump}
      canGoBack={focusNavigation.canGoBack}
      canGoForward={focusNavigation.canGoForward}
      canMutate={hostState.canMutate}
      onBack={() => void runCommand("focus.back")}
      onBell={() => void runCommand("agents.jumpUnread")}
      onForward={() => void runCommand("focus.forward")}
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
        activePaneId={activePane?.id}
        adapters={agentRuntime.adapters}
        agents={visibleAgentRows}
        agentSort={appState.shell.agentSort}
        agentsRatio={appState.shell.agentsSectionRatio}
        compactWorkspaces={appState.shell.compactWorkspaces}
        hookNotice={agentHostSetup.notice}
        hosts={sidebarHosts}
        onSetUpHost={agentHostSetup.offerable ? agentHostSetup.offer : undefined}
        onAgentsRatio={(ratio) => updateShell({ agentsSectionRatio: clampedAgentsRatio(ratio) })}
        maxWidth={Math.max(SIDEBAR_MIN_WIDTH, Math.floor(windowWidth / 3))}
        onWidth={(width) => updateShell({ sidebarWidth: sidebarWidthForWindow(width, windowWidth) })}
        width={sidebarWidth}
        onLaunchAgent={agentWorkflow.launch}
        onOpenSettings={() => setSettingsOpen(true)}
        // The rename names the agent where it lives, on the active host or
        // beside it; the runtime routes it by the agent's own host.
        onRenameAgent={(agent, scope) => {
          if (!scopeIsLive(scope)) return;
          setTextPrompt({
            title: "Rename agent",
            label: "Agent name",
            initialValue: agent.displayName,
            submit: (name) => {
              setTextPrompt(undefined);
              if (scopeIsLive(scope)) agentWorkflow.rename(agent, name);
            },
          });
        }}
        // A resume opens a new pane beside the one on screen, under the
        // active root the explorer has — both facts of the host on screen.
        // An agent on another host is resumed from there, once it is.
        onResumeAgent={(agent, placement, scope) => {
          if (!scopeIsLive(scope)) return;
          if (agent.hostProfileId !== currentHostProfileId) {
            const label = sidebarHosts.find((host) => host.profileId === agent.hostProfileId)?.label ?? agent.hostProfileId;
            return setStatus(`Resume opens a pane on the host on screen; select a workspace on ${label} first.`);
          }
          agentWorkflow.resume(agent, placement);
        }}
        onReviewHooks={agentWorkflow.reviewHooks}
        onSelectAgent={selectAgentRow}
        onSelectWorkspace={selectWorkspaceRow}
        onTogglePinnedOnly={() => void runCommand(appState.shell.pinnedOnly ? "workspaces.showAll" : "workspaces.showPinnedOnly")}
        onTogglePinnedAgentTab={toggleAgentTabPin}
        onTogglePinnedWorkspace={toggleWorkspacePin}
        onToggleShown={toggleHostShown}
        onSortMode={(mode) => updateShell({ agentSort: mode })}
        onWorkspaceCommand={(row, commandId) => void runCommand(commandId, { kind: "session", id: row.session.id, scope: row.scope })}
        pinnedOnly={appState.shell.pinnedOnly}
        rows={sidebarRows}
        stateGlyphs={appState.shell.agentStateGlyphs}
      />}
      <section className="workspace" aria-label={activeSession ? `Workspace ${activeSession.name}` : "Workspace"}>
        <TabStrip
          activeKey={activeCombinedTabKey}
          activePaneId={activePane?.id}
          activeTerminalPaneCount={panes.length}
          canMutate={hostState.canMutate && Boolean(activeSession)}
          commandScope={currentHostScope}
          onClose={closeCombinedTab}
          onCloseCurrent={(paneId, scope) => void runCommand("window.close", { kind: "focusedSurface", paneId, scope })}
          onCloseOthers={(tab, scope) => bulkCloseTabs(tabsToCloseOthers(combinedTabs, tab.key), scope)}
          onCloseNonAgent={(scope) => bulkCloseTabs(tabsToCloseNonAgent(combinedTabs), scope, true)}
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
            if (root) void startDownloadFlow({ path: appTab.resource, kind: "file" }, root, "tabMenu");
          }}
          onMove={moveCombinedTab}
          onNewTerminal={() => void runCommand("window.new")}
          onPin={(tab) => pinOpenTab(tab.id)}
          onTogglePinned={(tab) => toggleTabPin(tab, currentHostScope)}
          onRenameTerminal={(tab, scope) => void runCommand("window.rename", { kind: "terminalTab", id: tab.id, scope })}
          onSelect={selectCombinedTab}
          platform={platform}
          shortcuts={shortcuts}
          stateGlyphs={appState.shell.agentStateGlyphs}
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
            {/* Keyed by host: pane ids repeat across tmux servers, so a host
                switch remounts every pane rather than handing one machine's
                pane the other's. */}
            <TerminalWorkspaceSurface
              key={currentHostProfileId}
              activePane={activePane}
              activeWindow={activeWindow}
              appFocused={appFocused}
              beginDividerDrag={beginDividerDrag}
              cacheScope={currentHostProfileId}
              clientId={clientId}
              copyOnSelect={appState.shell.copyOnSelect}
              cleanWrappedCommands={appState.shell.cleanWrappedCommands}
              terminalApplicationClipboard={appState.shell.terminalApplicationClipboard}
              terminalFontSize={appState.shell.terminalFontSize}
              controllers={controllers}
              focusPane={focusTerminalPane}
              onMeasurements={onMeasurements}
              onOpenFilePath={(paneId, path) => { void openTerminalFilePath(paneId, path); }}
              grid={grid}
              handleInput={handleInput}
              handleKeyActivity={handleKeyActivity}
              hub={hub}
              mountedPanes={mountedPanes}
              onPaintSample={handlePaintSample}
              paneAttention={agentRuntime.rollups.byPane}
              panes={panes}
              platform={platform}
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
              onDownload={(path, kind, root) => void startDownloadFlow({ path, kind }, root, "fileSurface")}
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
        onDownload={async (intent) => { if (workspaceFiles.root) await startDownloadFlow(intent, workspaceFiles.root, "explorer"); }}
        onGitDiff={(entry, target, options) => {
          if (!activeSession || !hostState.serverIdentity || !workspaceFiles.root || !workspaceGit.status) return;
          const session = activeSession;
          const serverIdentity = hostState.serverIdentity;
          const root = workspaceFiles.root;
          const gitStatus = workspaceGit.status;
          const hostProfileId = currentHostProfileId;
          // The commit can run twice — once now, and again when a remote flight
          // it interrupted settles. The second run must only re-select the tab
          // the first one opened, never open it again: a diff the user has
          // closed in the meantime stays closed. Decided in the commit, not in
          // the updater: StrictMode runs the updater twice and keeps the
          // second result.
          let committed = false;
          shellNavigation.selectLocalAppTab(
            session.id,
            activeWindowId,
            `git:${gitStatus.repository.id}:${target}:${entry.path}`,
            () => {
              const replay = committed;
              committed = true;
              setAppState((current) => {
                if (!replay) {
                  return openGitDiffTab(
                    current, hostProfileId, serverIdentity, session, entry, target, gitStatus, root, options,
                  );
                }
                const opened = findGitDiffTab(
                  current, hostProfileId, serverIdentity, session.id, gitStatus.repository.id, entry.path, target,
                );
                return opened ? selectAppTab(current, hostProfileId, serverIdentity, session, opened.id) : current;
              });
            },
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
      activeProfileId={currentHostProfileId}
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
      onProfileFields={(patch) => saveProfileFields(selectedProfileId, patch)}
      onRequestHelperInstall={() => dispatchHelper({ type: "requestUpgrade" })}
      onShell={updateShell}
      onSounds={(preferences) => { setAgentSounds(preferences); saveAgentSoundPreferences(preferences); }}
      onWorkspaceDefaults={(patch) => setAppState((current) => setWorkspaceDefaults(current, currentHostProfileId, patch))}
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
      // The host the app is actually on, not the one the connection form is
      // editing: these defaults are applied by the next workspace created here,
      // and a form the user is filling in for a machine they have not connected
      // to yet is not that host.
      workspaceDefaults={workspaceDefaultsFor(appState, currentHostProfileId)}
      workspaceDefaultsHostId={currentHostProfileId}
      workspaceDefaultsHostLabel={currentHostLabel}
    />}
    {workspaceSwitcherOpen && <WorkspaceSwitcher
      onClose={() => setWorkspaceSwitcherOpen(false)}
      onSelect={selectWorkspaceRow}
      // Every workspace, filtered or not. The filter is a way to quieten the
      // list, not a way to make a workspace unreachable; leaving the switcher
      // narrowed would mean the only way back to an unpinned workspace is to
      // turn the filter off first.
      rows={switcherRows}
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
        // Against the host the row named, which need not be the one on
        // screen by the time the dialog is answered.
        const host = commandHostForScope(pending.scope);
        if (!host) return setStatus(`Closing ${pending.targetLabel} was cancelled because its host connection changed.`);
        void host.performAction(pending.action, pending.precondition);
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
