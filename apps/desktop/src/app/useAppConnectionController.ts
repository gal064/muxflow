import { invoke } from "@tauri-apps/api/core";
import { useEffect, useMemo, useReducer, useRef, useState, type Dispatch, type SetStateAction } from "react";
import type { TauriAgentClient } from "../features/agents/api";
import type { TauriFileWorkspaceClient } from "../features/files/api";
import type { TauriGitWorkspaceClient } from "../features/git/api";
import { helperConnectionKey } from "../features/shell/helperUpgrade";
import { hostProfileId } from "../features/shell/types";
import { useDesktopResumeRecovery } from "../features/shell/useDesktopResumeRecovery";
import { TerminalEventHub } from "../features/terminal/TerminalEventHub";
import { requestTerminalSeed, startTerminal, stopTerminal, terminalBridgeKey, terminalBridgeScope } from "../features/terminal/api";
import { terminalStateCache } from "../features/terminal/TerminalStateCache";
import { connectionReducer, denormalizeSnapshot, initialHostState } from "../state/connectionReducer";
import type { ConnectionSpec, HostProfile, PersistedProfiles } from "./types";
import { resolveActiveWindowId, type OptimisticWindowSwitch } from "./windowSelection";
import { resolveSelectedSession } from "../features/shell/model";
import type { HostScopeToken } from "../features/shell/hostScope";
import { recordPerfCounter } from "../perf/probe";

type ControllerArguments = {
  agentClient: TauriAgentClient;
  fileClient: TauriFileWorkspaceClient;
  gitClient: TauriGitWorkspaceClient;
  setStatus: Dispatch<SetStateAction<string>>;
};

export function useAppConnectionController({ agentClient, fileClient, gitClient, setStatus }: ControllerArguments) {
  const [hostState, dispatchHost] = useReducer(connectionReducer, initialHostState);
  const snapshot = useMemo(() => denormalizeSnapshot(hostState), [hostState]);
  const snapshotRef = useRef(snapshot);
  snapshotRef.current = snapshot;
  const [activeSessionId, setActiveSessionId] = useState<string>();
  const [activeWindowId, setActiveWindowId] = useState<string>();
  const [clientId, setClientId] = useState<string>();
  /**
   * Which host profile `clientId` was established for.
   *
   * `currentHostProfileId` is derived from the *pending* connection spec, and
   * the bridge is torn down by an effect, so a render that changes the
   * connection commits with the new profile id and the previous connection's
   * live client. Anything that acts on "the current host" in that window acts
   * on two different machines at once — M13-E004 is what that costs when the
   * act is writing an agent's configuration file. Recorded here, at the one
   * moment both facts are known together, so the mismatch is detectable
   * instead of depending on every caller remembering to reset host state
   * first.
   */
  const [clientHostProfileId, setClientHostProfileId] = useState<string>();
  const clientIdRef = useRef<string | undefined>(undefined);
  const [connectionEpoch, setConnectionEpoch] = useState(0);
  const [terminalEpoch, setTerminalEpoch] = useState(0);
  const terminalEpochRef = useRef(0);
  const [connection, setConnection] = useState<ConnectionSpec>({ mode: "local" });
  const [connectionMode, setConnectionMode] = useState<"local" | "ssh">("local");
  const [sshTarget, setSshTarget] = useState("");
  const [sshConfigPath, setSshConfigPath] = useState("");
  const [profiles, setProfiles] = useState<HostProfile[]>([]);
  /**
   * The saved host the *picker* is showing, which is not the one the app is
   * connected to.
   *
   * The picker used to derive its value by matching each saved profile against
   * the live `connection`, so choosing a different host filled the form in and
   * then snapped the control straight back to the connected one — the selection
   * was invisible until Connect made it the connection. Selecting is its own
   * state; Connect is what turns it into a connection. Empty means "current
   * values": either nothing is chosen, or the form has been edited away from
   * whatever was.
   */
  const [selectedProfileId, setSelectedProfileId] = useState("");
  const [profilesHydrated, setProfilesHydrated] = useState(false);
  const [profileRecovery, setProfileRecovery] = useState<PersistedProfiles["recovery"]>();
  const [connectionDetail, setConnectionDetail] = useState("");
  const [appFocused, setAppFocused] = useState(() => typeof document === "undefined" || document.hasFocus());
  const frontendResyncActive = useRef(false);
  const serverIdentityRef = useRef<string | undefined>(undefined);
  const currentHostProfileId = hostProfileId(connection);
  const currentHostScope: HostScopeToken = {
    hostProfileId: currentHostProfileId,
    connectionKey: helperConnectionKey(connection),
    connectionEpoch,
    serverIdentity: hostState.serverIdentity,
    generation: hostState.generation,
  };
  const hostScopeRef = useRef(currentHostScope);
  hostScopeRef.current = currentHostScope;
  const hub = useMemo(() => new TerminalEventHub(
    (paneId, reason) => {
      terminalStateCache.delete(paneId);
      const currentClientId = clientIdRef.current;
      if (!currentClientId) return;
      void requestTerminalSeed(currentClientId, paneId).catch((error) => {
        setStatus(`${reason}; seed request failed: ${String(error)}`);
      });
    },
    {},
    undefined,
    (message) => {
      recordPerfCounter("connection.reconnect.observerFailure");
      setConnectionDetail(message);
      setStatus(message);
      setConnectionEpoch((value) => value + 1);
    },
  ), [setStatus]);

  useDesktopResumeRecovery(() => {
    if (!profilesHydrated) return;
    recordPerfCounter("connection.reconnect.desktopResume");
    setConnectionDetail("System resumed; reconnecting for an authoritative state refresh.");
    setStatus("System resumed; reconnecting…");
    setConnectionEpoch((value) => value + 1);
  });

  useEffect(() => {
    const focused = () => setAppFocused(true);
    const blurred = () => setAppFocused(false);
    window.addEventListener("focus", focused);
    window.addEventListener("blur", blurred);
    return () => {
      window.removeEventListener("focus", focused);
      window.removeEventListener("blur", blurred);
    };
  }, []);

  useEffect(() => {
    if (hostState.resyncRequested && !frontendResyncActive.current) {
      frontendResyncActive.current = true;
      recordPerfCounter("connection.reconnect.sequenceGap");
      setConnectionEpoch((value) => value + 1);
    } else if (!hostState.resyncRequested) {
      frontendResyncActive.current = false;
    }
  }, [hostState.resyncRequested]);

  useEffect(() => {
    void invoke<PersistedProfiles>("list_host_profiles").then((saved) => {
      setProfiles(saved.profiles);
      setProfileRecovery(saved.recovery);
      const selected = saved.profiles.find((profile) => profile.id === saved.lastProfileId);
      if (!selected) return;
      const selectedConnection = selected.connection.mode === "ssh"
        ? { ...selected.connection, profileId: selected.connection.profileId || selected.id }
        : selected.connection;
      setConnection(selectedConnection);
      setConnectionMode(selected.connection.mode);
      setSelectedProfileId(selected.id);
      if (selected.connection.mode === "ssh") {
        setSshTarget(selected.connection.target);
        setSshConfigPath(selected.connection.configPath ?? "");
      }
    }).catch((error) => setStatus(String(error))).finally(() => setProfilesHydrated(true));
  }, [setStatus]);

  const windows = useMemo(() => snapshot.windows
    .filter((tmuxWindow) => tmuxWindow.sessionId === activeSessionId)
    .sort((a, b) => a.index - b.index), [activeSessionId, snapshot.windows]);
  /**
   * A window switch that has been committed locally and is still catching up.
   *
   * Owned here because this is where snapshots decide the active window, and
   * written by `useShellNavigation`, which is the only thing that knows a
   * switch is outstanding. See `OptimisticWindowSwitch`.
   */
  const optimisticWindow = useRef<OptimisticWindowSwitch | undefined>(undefined);
  useEffect(() => {
    const pending = optimisticWindow.current;
    if (pending) {
      // Released once the host has caught up: a snapshot at or past the
      // generation the select-window action returned is one that has seen the
      // switch, so from here the host's own answer is the better one — and an
      // unreleased guard would leave the shell ignoring tmux forever.
      const settled = pending.throughGeneration !== undefined
        && hostState.generation >= pending.throughGeneration;
      if (settled || pending.sessionId !== activeSessionId) optimisticWindow.current = undefined;
    }
    const preferred = optimisticWindow.current?.sessionId === activeSessionId
      ? optimisticWindow.current?.windowId
      : undefined;
    setActiveWindowId((current) => resolveActiveWindowId(windows, current, preferred));
  }, [activeSessionId, hostState.generation, snapshot.windows]);

  const bridgeKey = terminalBridgeKey(connection, connectionEpoch);
  useEffect(() => {
    if (!profilesHydrated) return;
    let disposed = false;
    let startedClient: string | undefined;
    let recoveringFlowStall = false;
    const scope = terminalBridgeScope();
    void startTerminal(scope.sessionId, scope.paneIds, connection, (event) => {
      if (disposed) return;
      const admission = hub.publish(event, () => {
        if (event.kind === "generationEpoch") {
          terminalStateCache.clear();
          terminalEpochRef.current = event.epoch;
          setTerminalEpoch(event.epoch);
        } else if (event.kind === "topologyDirty") {
          setStatus("Topology changed; reconciling…");
        } else if (event.kind === "flowStalled") {
          terminalStateCache.delete(event.paneId);
          // The host already tried the only in-place tmux resume twice. Its
          // fallback seed can repaint the last screen, but cannot restart the
          // stream after that budget is exhausted — exactly the pane that
          // appears frozen until a tab remount changes its attachment. Replace
          // the attachment authoritatively instead. One bridge may report more
          // than one affected pane, so collapse all of them into one reconnect.
          if (!recoveringFlowStall) {
            recoveringFlowStall = true;
            recordPerfCounter("connection.reconnect.terminalFlowStall");
            setConnectionDetail("A terminal output stream stalled; reconnecting it now.");
            setStatus("Terminal output stalled; reconnecting…");
            setConnectionEpoch((value) => value + 1);
          }
        } else if (event.kind === "error" || event.kind === "exit") {
          const detail = event.kind === "error" ? event.message : `Detached: ${event.reason}`;
          setConnectionDetail(detail);
          setStatus(detail);
        } else if (event.kind === "connectionState") {
          dispatchHost({ type: "connection", phase: event.state });
          if (event.state === "connected") setConnectionDetail("");
          setStatus(event.state === "connected" ? "Live" : `Connection ${event.state}…`);
        } else if (event.kind === "snapshot") {
          if (serverIdentityRef.current !== undefined && serverIdentityRef.current !== event.serverIdentity) {
            terminalStateCache.clear();
            hub.clearTerminalState();
          }
          serverIdentityRef.current = event.serverIdentity;
          dispatchHost({ type: "snapshot", snapshot: event.snapshot, sequence: event.sequence, generation: event.generation, serverIdentity: event.serverIdentity });
          setActiveSessionId((current) => resolveSelectedSession(
            event.snapshot.sessions,
            current,
            snapshotRef.current.sessions.find((session) => session.id === current)?.name,
          )?.id);
          setStatus("Live");
        } else if (event.kind === "fileService") {
          fileClient.publishWireEvent(event.event);
        } else if (event.kind === "gitService") {
          gitClient.publishWireEvent(event.event);
        } else if (event.kind === "agentService") {
          const serverIdentity = serverIdentityRef.current;
          if (!serverIdentity) return;
          const agentScope = {
            clientId: clientIdRef.current ?? "",
            hostProfileId: currentHostProfileId,
            serverIdentity,
            topologyGeneration: hostScopeRef.current.generation,
            connectionEpoch: terminalEpochRef.current,
          };
          if (event.snapshot) agentClient.publishWireSnapshot(agentScope, event.snapshot);
          else if (event.event) agentClient.publishWireEvent(agentScope, event.event);
        }
      });
      if (admission.kind === "gap") {
        dispatchHost({ type: "sequenceGap", expected: admission.expected, received: admission.received });
        setStatus(`Terminal event gap: expected ${admission.expected}, received ${admission.received}; resyncing…`);
      }
    }).then((id) => {
      startedClient = id;
      if (disposed) void stopTerminal(id);
      else {
        clientIdRef.current = id;
        setClientId(id);
        setClientHostProfileId(hostProfileId(connection));
      }
    }).catch((error) => { if (!disposed) setStatus(String(error)); });

    return () => {
      disposed = true;
      if (clientIdRef.current === startedClient) clientIdRef.current = undefined;
      dispatchHost({ type: "connection", phase: "disconnected" });
      setClientId(undefined);
      setClientHostProfileId(undefined);
      terminalEpochRef.current = 0;
      setTerminalEpoch(0);
      if (startedClient) void stopTerminal(startedClient);
    };
  }, [bridgeKey, profilesHydrated]);

  return {
    activeSessionId, activeWindowId, appFocused, clientHostProfileId, clientId, clientIdRef, connection,
    connectionDetail, connectionEpoch, connectionMode, currentHostProfileId,
    currentHostScope, dispatchHost, hostScopeRef, hostState, hub, profileRecovery,
    optimisticWindow,
    profiles, profilesHydrated, selectedProfileId, setActiveSessionId, setActiveWindowId,
    setConnection, setConnectionDetail, setConnectionEpoch, setConnectionMode,
    setProfileRecovery, setProfiles, setSelectedProfileId, setSshConfigPath, setSshTarget,
    snapshot, snapshotRef, sshConfigPath, sshTarget, terminalEpoch, terminalEpochRef, windows,
  };
}
