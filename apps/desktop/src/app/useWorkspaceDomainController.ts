import { useMemo } from "react";
import type { TauriFileWorkspaceClient } from "../features/files/api";
import type { FileWorkspaceScope } from "../features/files/types";
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
  gitRepositories: GitRepositoryStore;
  serverIdentity?: string;
  snapshot: TmuxSnapshot;
  terminalEpoch: number;
  windows: Window[];
};

export function useWorkspaceDomainController(arguments_: WorkspaceDomainArguments) {
  const {
    activeSessionId, activeWindowId, appState, clientId, connection,
    currentHostProfileId, fileClient, generation, gitRepositories, serverIdentity,
    snapshot, terminalEpoch, windows,
  } = arguments_;
  const panes = useMemo(
    () => snapshot.panes.filter((pane) => pane.windowId === activeWindowId),
    [activeWindowId, snapshot.panes],
  );
  const activePane: Pane | undefined = panes.find((pane) => pane.active) ?? panes[0];
  const activeSession: Session | undefined = snapshot.sessions.find((session) => session.id === activeSessionId);
  const activeWindow = windows.find((tmuxWindow) => tmuxWindow.id === activeWindowId);
  const fileScope = useMemo<FileWorkspaceScope | undefined>(() => clientId && terminalEpoch && serverIdentity && activeSession && activePane ? {
    clientId,
    hostProfileId: currentHostProfileId,
    serverIdentity,
    generation,
    terminalEpoch,
    sessionId: activeSession.id,
    paneId: activePane.id,
  } : undefined, [activePane?.id, activeSession?.id, clientId, currentHostProfileId, generation, serverIdentity, terminalEpoch]);
  const terminalTransferScope = useMemo<TerminalTransferConnectionScope | undefined>(() => clientId && terminalEpoch && serverIdentity ? {
    clientId,
    hostProfileId: currentHostProfileId,
    serverIdentity,
    connectionEpoch: String(terminalEpoch),
    mode: connection.mode,
  } : undefined, [clientId, connection.mode, currentHostProfileId, serverIdentity, terminalEpoch]);
  const workspaceFiles = useWorkspaceFiles(fileClient, fileScope);
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
