import { invoke } from "@tauri-apps/api/core";
import { lazy, Suspense, useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import type { PendingTextPrompt } from "../commands/TextInputDialog";
import type { PendingTmuxConfirmation } from "../commands/destructiveConfirmation";
import {
  commandAvailable,
  commandForKeyboardEvent,
  commandsForSurface,
  currentPlatform,
  globalShortcutAllowed,
  shortcutFor,
  type ShortcutOverrides,
} from "../commands/registry";
import type { TerminalPaneController } from "../features/terminal/TerminalPane";
import { adjacentPane, resizeCellsFromPixels, windowGrid, type PaneDirection } from "../features/terminal/layout";
import { resizeClient, sendBinaryInput, sendInput } from "../features/terminal/api";
import type { TerminalInput, TerminalSize } from "../features/terminal/TerminalRenderer";
import { TauriTerminalTransferClient } from "../features/terminal/terminalTransferApi";
import { TerminalTransferHistory } from "../features/terminal/TerminalTransferSurface";
import { useTerminalTransferRegistry } from "../features/terminal/terminalTransferRegistry";
import { requestTmuxAction, type TmuxAction } from "../features/tmux/actions";
import { requestReconciledTmuxAction } from "../features/tmux/actionReconciliation";
import { AgentPanel } from "../features/agents/AgentPanel";
import { TauriAgentClient } from "../features/agents/api";
import { loadAgentSoundPreferences } from "../features/agents/sound";
import { useAgentNotificationActivation, type PaneSurfaceResult } from "../features/agents/useAgentNotificationActivation";
import { useAgentRuntime } from "../features/agents/useAgentRuntime";
import { keyForScope, keyForTransferConnection, TauriFileWorkspaceClient } from "../features/files/api";
import { ExplorerTree } from "../features/files/ExplorerTree";
import type { PendingDownload } from "../features/files/DownloadDialog";
import type { ActiveRoot, DownloadRequest, FileEntry, FileMutation } from "../features/files/types";
import { TauriGitWorkspaceClient } from "../features/git/api";
import { GitSidebar } from "../features/git/GitSidebar";
import { ConnectionBanner } from "../features/shell/ConnectionBanner";
import { ExplorerGitSidebar } from "../features/shell/ExplorerGitSidebar";
import { helperConnectionKey, helperUpgradeReducer, initialHelperUpgradeState, type HelperInstallReport, type RemoteHelperProbe } from "../features/shell/helperUpgrade";
import { profileIdForSshConnection } from "../features/shell/hostProfiles";
import { sameHostConnection, sameHostScope, type HostScopeToken } from "../features/shell/hostScope";
import { useShellCommands } from "../features/shell/useShellCommands";
import { usePersistedAppState } from "../features/shell/usePersistedAppState";
import {
  combineWorkspaceTabs,
  discardServerAppState,
  mountedTerminalPanes,
  openFileTab,
  openGitDiffTab,
  reconcileWorkspaceIdentity,
  recoverableAppTabCount,
  recoverAppTabsFromPreviousServer,
  selectAppTab,
  setMarkdownViewMode,
  shouldSurfaceAuthoritativeTerminal,
  shellNavigationMode,
  type CombinedTab,
} from "../features/shell/model";
import { CombinedTabStrip, workspaceTabDomId, workspaceTabPanelDomId } from "../features/workspaces/CombinedTabStrip";
import { WorkspaceRail } from "../features/workspaces/WorkspaceRail";
import type { ConnectionSpec, HostProfile, Pane, PersistedProfiles } from "./types";
import { resolveTerminalDestination } from "./paneRouting";
import { requestActiveWindow } from "./windowSelection";
import { useAppConnectionController } from "./useAppConnectionController";
import { useWorkspaceDomainController } from "./useWorkspaceDomainController";
import { AppDialogLayer } from "./AppDialogLayer";
import { TerminalWorkspaceSurface } from "./TerminalWorkspaceSurface";

const AppTabSurface = lazy(() => import("../features/shell/AppTabSurface").then((module) => ({ default: module.AppTabSurface })));
const GitDiffSurface = lazy(() => import("../features/git/GitDiffSurface").then((module) => ({ default: module.GitDiffSurface })));

export function App() {
  const [status, setStatus] = useState("Discovering local tmux…");
  const agentClient = useMemo(() => new TauriAgentClient(), []);
  const fileClient = useMemo(() => new TauriFileWorkspaceClient(), []);
  const gitClient = useMemo(() => new TauriGitWorkspaceClient(), []);
  const connectionController = useAppConnectionController({ agentClient, fileClient, gitClient, setStatus });
  const {
    activeSessionId, activeWindowId, appFocused, clientId, clientIdRef, connection,
    connectionDetail, connectionEpoch, connectionMode, currentHostProfileId,
    currentHostScope, dispatchHost, hostScopeRef, hostState, hub, profileRecovery,
    profiles, profilesHydrated, resizeTimer, setActiveSessionId, setActiveWindowId,
    setConnection, setConnectionDetail, setConnectionEpoch, setConnectionMode,
    setProfileRecovery, setProfiles, setSshConfigPath, setSshTarget, snapshot,
    snapshotRef, sshConfigPath, sshTarget, terminalEpoch, windows,
  } = connectionController;
  const { appState, appStateRecovery, resetAppState, setAppState } = usePersistedAppState(setStatus);
  const [helperState, dispatchHelper] = useReducer(helperUpgradeReducer, initialHelperUpgradeState);
  const [profileResetConfirmation, setProfileResetConfirmation] = useState(false);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [shortcutEditorOpen, setShortcutEditorOpen] = useState(false);
  const [confirmation, setConfirmation] = useState<PendingTmuxConfirmation>();
  const [textPrompt, setTextPrompt] = useState<PendingTextPrompt>();
  const [appStateResetConfirmation, setAppStateResetConfirmation] = useState(false);
  const [appRecoveryDiscardConfirmation, setAppRecoveryDiscardConfirmation] = useState(false);
  const [pendingAppRecovery, setPendingAppRecovery] = useState<{ hostProfileId: string; previousServerIdentity: string; currentServerIdentity: string; count: number; scope: HostScopeToken }>();
  const [pendingDownload, setPendingDownload] = useState<PendingDownload>();
  const [agentSounds, setAgentSounds] = useState(loadAgentSoundPreferences);
  const [agentModalOpen, setAgentModalOpen] = useState(false);
  const controllers = useRef(new Map<string, TerminalPaneController>());
  const platform = useMemo(() => currentPlatform(), []);
  const shortcuts = appState.commands.shortcutOverrides as ShortcutOverrides;
  const currentHelperConnectionKey = helperConnectionKey(connection);
  const terminalTransferClient = useMemo(() => new TauriTerminalTransferClient(), []);
  const terminalTransferRegistry = useTerminalTransferRegistry();

  useEffect(() => dispatchHelper({ type: "reset" }), [currentHelperConnectionKey]);

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
  const performAction = useCallback(async (
    action: TmuxAction,
    capturedPrecondition?: { serverIdentity: string; generation: number },
  ) => {
    if (!clientId || !hostState.canMutate || !hostState.serverIdentity) {
      setStatus("This action is unavailable until the authoritative connection is live.");
      return false;
    }
    try {
      await requestReconciledTmuxAction({
        clientId,
        action,
        capturedPrecondition,
        initialScope: hostScopeRef.current,
        currentScope: () => hostScopeRef.current,
      });
      setStatus("Waiting for authoritative tmux state…");
      return true;
    } catch (error) {
      setStatus(String(error));
      return false;
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
      await requestTmuxAction(clientId, { kind: "focusPane", sessionId: target.sessionId, windowId: target.windowId, paneId: target.id }, {
        serverIdentity: hostState.serverIdentity,
        generation: hostState.generation,
      });
    } catch (error) {
      setStatus(String(error));
      return { ok: false, error };
    }
    if (!sameHostScope(scope, hostScopeRef.current)) return { ok: false, error: new Error("authoritative connection changed while focusing") };
    setAppState((current) => selectAppTab(current, currentHostProfileId, hostState.serverIdentity!, session, undefined));
    setActiveSessionId(target.sessionId);
    setActiveWindowId(target.windowId);
    setStatus(successMessage ? `${successMessage} Focus request accepted.` : `${source} focus request accepted for ${target.sessionId}/${target.windowId}/${target.id}.`);
    window.requestAnimationFrame(() => controllers.current.get(target.id)?.focus());
    return { ok: true };
  }, [clientId, currentHostProfileId, hostState.canMutate, hostState.generation, hostState.serverIdentity, setAppState]);

  const agentScope = useMemo(() => clientId && hostState.serverIdentity ? {
    clientId,
    hostProfileId: currentHostProfileId,
    serverIdentity: hostState.serverIdentity,
    topologyGeneration: hostState.generation,
    connectionEpoch: terminalEpoch,
  } : undefined, [clientId, currentHostProfileId, hostState.generation, hostState.serverIdentity, terminalEpoch]);
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

  const focusDirection = useCallback((direction: PaneDirection) => {
    if (!activePane) return;
    const target = adjacentPane(panes, activePane, direction);
    if (target) void performAction({ kind: "focusPane", paneId: target.id, windowId: target.windowId, sessionId: target.sessionId });
  }, [activePane, panes, performAction]);

  const { commandContext, runCommand } = useShellCommands({
    activePane, activeSession, activeWindow, appState, canMutate: hostState.canMutate,
    combinedTabs, controllers, currentHostProfileId, focusDirection,
    generation: hostState.generation, hostScope: currentHostScope,
    isHostScopeCurrent: (scope) => sameHostConnection(scope, hostScopeRef.current),
    performAction, selectedAppTab,
    serverIdentity: hostState.serverIdentity, setAppState, setConfirmation,
    setPaletteOpen, setShortcutEditorOpen, setStatus, setTextPrompt, snapshot, windows,
  });
  const modalOpen = paletteOpen || shortcutEditorOpen || Boolean(confirmation) || Boolean(textPrompt)
    || agentModalOpen || Boolean(pendingDownload) || appStateResetConfirmation || appRecoveryDiscardConfirmation || helperState.phase === "confirming";

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

  const handleResize = useCallback((pane: Pane, size: TerminalSize) => {
    if (!clientId || !hostState.canMutate || !pane.active || size.columns < 2 || size.rows < 2) return;
    window.clearTimeout(resizeTimer.current);
    resizeTimer.current = window.setTimeout(() => {
      const columns = Math.max(2, Math.round((size.columns * grid.width) / pane.width));
      const rows = Math.max(2, Math.round((size.rows * grid.height) / pane.height));
      void resizeClient(clientId, columns, rows).catch((error) => { if (clientIdRef.current === clientId) setStatus(String(error)); });
    }, 60);
  }, [clientId, grid.height, grid.width, hostState.canMutate]);

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

  const selectSession = (sessionId: string) => {
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
  };
  const selectWindow = (windowId: string) => {
    notificationActivation.clearNotificationFocusGuard();
    if (activeSession && hostState.serverIdentity) setAppState((current) => selectAppTab(current, currentHostProfileId, hostState.serverIdentity!, activeSession, undefined));
    if (shellNavigationMode(hostState.canMutate) === "cached") {
      setActiveWindowId(windowId);
      setStatus("Viewing the last known terminal tab. Writes remain frozen.");
      return;
    }
    void requestActiveWindow(windows, activeWindowId, windowId, performAction, setActiveWindowId);
  };

  const selectCombinedTab = (tab: CombinedTab) => {
    if (tab.kind === "terminal") selectWindow(tab.id);
    else if (activeSession && hostState.serverIdentity) {
      setAppState((current) => selectAppTab(current, currentHostProfileId, hostState.serverIdentity!, activeSession, tab.id));
      setStatus(`Opened ${tab.title}`);
    }
  };

  const closeCombinedTab = (tab: CombinedTab) => {
    void runCommand("window.close", { kind: tab.kind === "app" ? "appTab" : "terminalTab", id: tab.id });
  };

  const openExplorerEntry = (entry: FileEntry) => {
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
    ));
  };

  const mutateFile = async (mutation: FileMutation) => {
    if (!fileScope || !workspaceFiles.root || !hostState.canMutate) throw new Error("File changes are unavailable while the host is read-only.");
    try {
      await fileClient.mutate(fileScope, workspaceFiles.root, mutation);
      const directory = "parent" in mutation
        ? mutation.parent
        : mutation.path.slice(0, mutation.path.lastIndexOf("/")) || workspaceFiles.root.path;
      workspaceFiles.refresh(directory);
      setStatus(`File ${mutation.kind} completed.`);
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
      setStatus(`Download ${transfer.state}: ${request.path}`);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      workspaceFiles.recordTransfer({
        id: crypto.randomUUID(), scopeKey: keyForTransferConnection(fileScope), path: request.path, destination: request.destination, kind: request.kind,
        state: "failed", outcome: "notPublished", failureKind: "transfer", completedBytes: "0", filesCompleted: "0", error: message,
      });
      setStatus(message);
    }
  };

  const moveCombinedTab = (tab: CombinedTab, direction: "left" | "right") => {
    void runCommand(direction === "left" ? "window.moveLeft" : "window.moveRight", {
      kind: tab.kind === "app" ? "appTab" : "terminalTab",
      id: tab.id,
    });
  };

  const selectProfile = (profile: HostProfile) => {
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
    setProfiles((current) => [...current.filter((item) => item.id !== profile.id), profile]);
    void invoke("save_host_profile", { profile }).catch((error) => setStatus(String(error)));
    setStatus(`Connecting to ${target}…`);
  };

  const commandMenu = <details className="command-menu">
          <summary aria-label="Workspace and terminal actions">•••</summary>
          <div aria-label="Workspace and terminal commands">
            {(["Application", "View", "Workspace", "Terminal tab", "Pane", "Terminal"] as const).map((group) => <section key={group}>
              <small>{group}</small>
              {commandsForSurface("menu").filter((command) => command.group === group).map((command) => <button
                disabled={!commandAvailable(command, commandContext)}
                key={command.id}
                onClick={(event) => {
                  void runCommand(command.id);
                  (event.currentTarget.closest("details") as HTMLDetailsElement | null)?.removeAttribute("open");
                }}
                type="button"
              >{command.title}<kbd>{shortcutFor(command, platform, shortcuts) ?? ""}</kbd></button>)}
            </section>)}
          </div>
        </details>;

  return <main className={`app-shell ${appState.shell.explorerCollapsed ? "left-collapsed" : ""} ${appState.shell.agentSidebarCollapsed ? "agents-collapsed" : ""}`}>
    {profileRecovery && <div className="app-state-recovery" role="alert"><strong>Saved host profiles were recovered</strong><span>{profileRecovery.error} The original was preserved at {profileRecovery.preservedPath}.</span><button onClick={() => setProfileResetConfirmation(true)} type="button">Confirm recovered defaults…</button></div>}
    {appStateRecovery && <div className="app-state-recovery" role="alert"><strong>Saved shell state is write-frozen</strong><span>{appStateRecovery}</span><button onClick={() => setAppStateResetConfirmation(true)} type="button">Reset saved shell state…</button></div>}
    {pendingAppRecovery && <div className="app-tab-recovery" role="status"><strong>App tabs found from the replaced tmux server</strong><span>{pendingAppRecovery.count} tab{pendingAppRecovery.count === 1 ? "" : "s"} can be rebound by unique workspace name. Terminal and pane identities are never reused.</span><div><button onClick={() => {
      if (!sameHostScope(pendingAppRecovery.scope, hostScopeRef.current)) return setPendingAppRecovery(undefined);
      setAppState((current) => recoverAppTabsFromPreviousServer(current, pendingAppRecovery.hostProfileId, pendingAppRecovery.previousServerIdentity, pendingAppRecovery.currentServerIdentity, snapshot.sessions));
      setPendingAppRecovery(undefined);
    }} type="button">Restore app tabs</button><button onClick={() => setAppRecoveryDiscardConfirmation(true)} type="button">Discard old tabs…</button></div></div>}
    <WorkspaceRail
      activeSessionId={activeSessionId}
      attentionByWorkspace={agentRuntime.rollups.byWorkspace}
      canMutate={hostState.canMutate}
      onCommand={(session, commandId) => void runCommand(commandId, { kind: "session", id: session.id })}
      onCreate={() => void runCommand("session.new")}
      onSelect={selectSession}
      sessions={snapshot.sessions}
    />
    <ExplorerGitSidebar
      activePane={activePane}
      activeRoot={workspaceFiles.root?.path}
      collapsed={appState.shell.explorerCollapsed}
      connection={connection}
      connectionMode={connectionMode}
      onConnect={connect}
      onConnectionMode={setConnectionMode}
      onProfile={selectProfile}
      onSshConfigPath={setSshConfigPath}
      onSshTarget={setSshTarget}
      onSurface={(surface) => void runCommand(surface === "explorer" ? "view.showExplorer" : "view.showGit")}
      onToggleCollapsed={() => void runCommand("view.toggleExplorer")}
      profiles={profiles}
      sshConfigPath={sshConfigPath}
      sshTarget={sshTarget}
      surface={appState.shell.explorerSurface}
      explorer={<ExplorerTree
        disabled={!hostState.canMutate}
        error={workspaceFiles.error}
        expanded={workspaceFiles.expanded}
        listings={workspaceFiles.listings}
        loading={workspaceFiles.loading}
        onCancelTransfer={async (id) => { if (fileScope) await fileClient.cancelTransfer(fileScope, id); }}
        onDownload={async (request) => { if (workspaceFiles.root) setPendingDownload({ root: workspaceFiles.root, path: request.path, kind: request.kind }); }}
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
    />
    <section className="workspace" aria-label={activeSession ? `Workspace ${activeSession.name}` : "Workspace"}>
      <ConnectionBanner
        detail={connectionDetail || status}
        hasSnapshot={snapshot.sessions.length > 0}
        helper={helperState}
        onProbeHelper={() => void probeHelper()}
        onReconnect={() => setConnectionEpoch((value) => value + 1)}
        onRequestHelperInstall={() => dispatchHelper({ type: "requestUpgrade" })}
        phase={hostState.phase}
        remote={connection.mode === "ssh"}
      />
      <CombinedTabStrip
        activeKey={activeCombinedTabKey}
        canMutate={hostState.canMutate && Boolean(activeSession)}
        commandMenu={commandMenu}
        onClose={closeCombinedTab}
        onMove={moveCombinedTab}
        onNewTerminal={() => void runCommand("window.new")}
        onOpenPalette={() => void runCommand("commands.show")}
        onRenameTerminal={(tab) => void runCommand("window.rename", { kind: "terminalTab", id: tab.id })}
        onSelect={selectCombinedTab}
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
        {selectedAppTab ? <Suspense fallback={<div className="empty">Loading editor…</div>}>{selectedAppTab.kind === "gitDiff" ? <GitDiffSurface
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
          onDownload={(path, kind, root) => setPendingDownload({ path, kind, root })}
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
          grid={grid}
          handleInput={handleInput}
          handleResize={handleResize}
          hub={hub}
          mountedPanes={mountedPanes}
          panes={panes}
          performAction={performAction}
          setStatus={setStatus}
          snapshot={snapshot}
          terminalTransferClient={terminalTransferClient}
          terminalTransferRegistry={terminalTransferRegistry}
          terminalTransferScope={terminalTransferScope}
        />}
      </div>
    </section>
    <TerminalTransferHistory client={terminalTransferClient} onError={(error) => setStatus(String(error))} registry={terminalTransferRegistry} />
    <AgentPanel
      canMutate={hostState.canMutate}
      collapsed={appState.shell.agentSidebarCollapsed}
      launchContext={activeSession && activeWindow && activePane && workspaceFiles.root ? { sessionId: activeSession.id, windowId: activeWindow.id, paneId: activePane.id, root: workspaceFiles.root } : undefined}
      onModalChange={setAgentModalOpen}
      onSelect={(agent) => {
        notificationActivation.clearNotificationFocusGuard();
        if (!agent.paneId) return setStatus(`Agent ${agent.displayName} has no exact pane match; navigation is unavailable.`);
        const destination = resolveTerminalDestination(snapshot.panes, agent.paneId);
        if (destination.kind === "unavailable") return setStatus(`Agent destination ${agent.displayName} is no longer available: ${destination.reason}.`);
        void surfacePaneDestination(destination.pane, `Agent ${agent.displayName}`);
      }}
      onSoundPreferences={setAgentSounds}
      onStatus={setStatus}
      onToggle={() => void runCommand("view.toggleAgents")}
      runtime={agentRuntime}
      soundPreferences={agentSounds}
    />
    <AppDialogLayer
      appRecoveryDiscard={appRecoveryDiscardConfirmation ? pendingAppRecovery : undefined}
      appStateResetConfirmation={appStateResetConfirmation}
      commandContext={commandContext}
      confirmation={confirmation}
      helperState={helperState}
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
      onDownloadCancel={() => setPendingDownload(undefined)}
      onDownloadConfirm={(request, root) => {
        setPendingDownload(undefined);
        void startDownload(request, root);
      }}
      onHelperCancel={() => dispatchHelper({ type: "cancelUpgrade" })}
      onHelperConfirm={() => void confirmHelperInstall()}
      onPaletteClose={() => setPaletteOpen(false)}
      onProfileResetCancel={() => setProfileResetConfirmation(false)}
      onProfileResetConfirm={() => {
        setProfileResetConfirmation(false);
        void invoke("reset_host_profiles").then(() => {
          const local: HostProfile = { id: "local", label: "Local", connection: { mode: "local" } };
          setProfiles([local]);
          setProfileRecovery(undefined);
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
      pendingDownload={pendingDownload}
      platform={platform}
      profileResetConfirmation={profileResetConfirmation}
      shortcuts={shortcuts}
      shortcutEditorOpen={shortcutEditorOpen}
      textPrompt={textPrompt}
    />
    <div className="sr-only" aria-live="polite">{status}</div>
  </main>;
}
