import { useMemo } from "react";
import type { TauriFileWorkspaceClient } from "../features/files/api";
import { keyForWorkspaceSelection } from "../features/files/api";
import type { FileWorkspaceScope, FileWorkspaceSelection } from "../features/files/types";
import { useWorkspaceFiles } from "../features/files/useWorkspaceFiles";
import { useWorkspaceGit } from "../features/git/useWorkspaceGit";
import type { GitRepositoryStore } from "../features/git/repositoryStore";
import { appTabsForWorkspace, workspaceUiRecord } from "../features/shell/model";
import type { PersistedAppState } from "../features/shell/types";
import type { TerminalTransferConnectionScope } from "../features/terminal/terminalTransfers";
import type { ConnectionSpec, Pane, Session, TmuxSnapshot, Window } from "./types";

type WorkspaceDomainArguments = {
  activeSessionId?: string;
  activeWindowId?: string;
  appState: PersistedAppState;
  clientId?: string;
  connection: ConnectionSpec;
  currentHostProfileId: string;
  fileClient: TauriFileWorkspaceClient;
  generation: number;
  connected: boolean;
  gitRepositories: GitRepositoryStore;
  serverIdentity?: string;
  snapshot: TmuxSnapshot;
  terminalEpoch: number;
  windows: Window[];
};

/**
 * The native bridge publishes its snapshot before it publishes `connected`.
 * Keeping this boundary pure makes it impossible to accidentally recreate a
 * request-capable scope in that startup gap.
 */
export function liveFileScope(
  connected: boolean,
  clientId: string | undefined,
  terminalEpoch: number,
  generation: number,
  selection: FileWorkspaceSelection | undefined,
): FileWorkspaceScope | undefined {
  return connected && clientId && terminalEpoch && selection ? {
    clientId,
    ...selection,
    generation,
    terminalEpoch,
  } : undefined;
}

export function useWorkspaceDomainController(arguments_: WorkspaceDomainArguments) {
  const {
    activeSessionId, activeWindowId, appState, clientId, connection,
    connected, currentHostProfileId, fileClient, generation, gitRepositories, serverIdentity,
    snapshot, terminalEpoch, windows,
  } = arguments_;
  const panes = useMemo(
    () => snapshot.panes.filter((pane) => pane.windowId === activeWindowId),
    [activeWindowId, snapshot.panes],
  );
  const activePane: Pane | undefined = panes.find((pane) => pane.active) ?? panes[0];
  const activeSession: Session | undefined = snapshot.sessions.find((session) => session.id === activeSessionId);
  const activeWindow = windows.find((tmuxWindow) => tmuxWindow.id === activeWindowId);
  const fileSelection = useMemo<FileWorkspaceSelection | undefined>(() => serverIdentity && activeSession && activePane ? {
    hostProfileId: currentHostProfileId,
    serverIdentity,
    sessionId: activeSession.id,
    paneId: activePane.id,
  } : undefined, [activePane?.id, activeSession?.id, currentHostProfileId, serverIdentity]);
  const fileSelectionKey = fileSelection ? keyForWorkspaceSelection(fileSelection) : "";
  const fileScope = useMemo<FileWorkspaceScope | undefined>(
    () => liveFileScope(connected, clientId, terminalEpoch, generation, fileSelection),
    [clientId, connected, fileSelection, generation, terminalEpoch],
  );
  const terminalTransferScope = useMemo<TerminalTransferConnectionScope | undefined>(() => clientId && terminalEpoch && serverIdentity ? {
    clientId,
    hostProfileId: currentHostProfileId,
    serverIdentity,
    connectionEpoch: String(terminalEpoch),
    mode: connection.mode,
  } : undefined, [clientId, connection.mode, currentHostProfileId, serverIdentity, terminalEpoch]);
  const workspaceFiles = useWorkspaceFiles(fileClient, fileScope, fileSelectionKey);
  const workspaceGit = useWorkspaceGit(gitRepositories, fileScope, workspaceFiles.root);
  const workspaceAppTabs = useMemo(
    () => appTabsForWorkspace(appState, currentHostProfileId, serverIdentity, activeSession),
    [activeSession, appState, currentHostProfileId, serverIdentity],
  );
  const persistedWorkspaceUi = workspaceUiRecord(appState, currentHostProfileId, serverIdentity, activeSession);
  const selectedAppTab = workspaceAppTabs.find((tab) => tab.id === persistedWorkspaceUi?.selectedAppTabId);

  return {
    activePane, activeSession, activeWindow, fileScope, panes, selectedAppTab,
    terminalTransferScope, workspaceAppTabs, workspaceFiles, workspaceGit,
  };
}
