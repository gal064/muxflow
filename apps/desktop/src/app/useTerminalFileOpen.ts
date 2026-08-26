import { useCallback, type Dispatch, type MutableRefObject, type SetStateAction } from "react";
import type { FileWorkspaceScope, TerminalFileResolver } from "../features/files/types";
import { sameHostConnection, type HostScopeToken } from "../features/shell/hostScope";
import { openFileTab } from "../features/shell/model";
import type { AppTabViewMode, PersistedAppState } from "../features/shell/types";
import { currentTerminalFilePane } from "./terminalFileOpenRoute";
import type { TmuxSnapshot } from "./types";

interface TerminalFileOpenOptions {
  clientIdRef: MutableRefObject<string | undefined>;
  /**
   * The configured starting mode for a new Markdown tab, read when the tab is
   * created rather than when this hook is built: the resolve is a host round
   * trip, and the setting may be changed while it is in flight.
   */
  defaultMarkdownView(): AppTabViewMode;
  fileClient: TerminalFileResolver;
  fileScope?: FileWorkspaceScope;
  hostScopeRef: MutableRefObject<HostScopeToken>;
  selectLocalAppTab(
    sessionId: string,
    windowId: string | undefined,
    appTabId: string,
    commitLocal: () => void,
  ): void;
  setAppState: Dispatch<SetStateAction<PersistedAppState>>;
  setStatus(status: string): void;
  snapshotRef: MutableRefObject<TmuxSnapshot>;
}

/** Resolves an application-emitted terminal path without crossing host or pane generations. */
export function useTerminalFileOpen(options: TerminalFileOpenOptions) {
  const {
    clientIdRef, defaultMarkdownView, fileClient, fileScope, hostScopeRef, selectLocalAppTab,
    setAppState, setStatus, snapshotRef,
  } = options;
  return useCallback(async (paneId: string, candidate: string): Promise<void> => {
    const pane = snapshotRef.current.panes.find((item) => item.id === paneId);
    const capturedHost = hostScopeRef.current;
    const liveClientId = clientIdRef.current;
    if (!pane || !fileScope || !liveClientId || !capturedHost.serverIdentity) {
      setStatus("Reconnect the terminal before opening a file path.");
      return;
    }
    const scope = {
      ...fileScope,
      clientId: liveClientId,
      paneId,
      sessionId: pane.sessionId,
    };
    try {
      const resolved = await fileClient.resolveTerminalFile(scope, candidate, {
        sessionId: pane.sessionId,
        windowId: pane.windowId,
        cwd: pane.currentPath,
      });
      if (clientIdRef.current !== liveClientId
        || !sameHostConnection(capturedHost, hostScopeRef.current)) return;
      const livePane = currentTerminalFilePane(
        pane,
        snapshotRef.current.panes,
        scope.generation,
        resolved.topologyGeneration,
      );
      if (!livePane) {
        setStatus(`Could not open ${candidate}: that pane moved while the path was resolving.`);
        return;
      }
      const session = snapshotRef.current.sessions.find((item) => item.id === livePane.sessionId);
      if (!session) {
        setStatus(`Could not open ${candidate}: its session is no longer attached.`);
        return;
      }
      const kind = /\.md(?:own)?$/i.test(resolved.path) ? "markdown" as const : "file" as const;
      selectLocalAppTab(
        session.id,
        livePane.windowId,
        `file:${resolved.root.token}:${resolved.path}`,
        () => {
          setAppState((current) => openFileTab(
            current,
            capturedHost.hostProfileId,
            capturedHost.serverIdentity!,
            session,
            resolved.path,
            kind,
            resolved.root,
            { preview: false, viewMode: defaultMarkdownView() },
          ));
        },
      );
    } catch (error) {
      if (clientIdRef.current === liveClientId
        && sameHostConnection(capturedHost, hostScopeRef.current)) {
        setStatus(`Could not open ${candidate}: ${String(error)}`);
      }
    }
  }, [clientIdRef, defaultMarkdownView, fileClient, fileScope, hostScopeRef, selectLocalAppTab,
    setAppState, setStatus, snapshotRef]);
}
