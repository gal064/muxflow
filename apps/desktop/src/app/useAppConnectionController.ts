import { invoke } from "@tauri-apps/api/core";
import { useEffect, useMemo, useReducer, useRef, useState, type Dispatch, type SetStateAction } from "react";
import type { TauriAgentClient } from "../features/agents/api";
import type { TauriFileWorkspaceClient } from "../features/files/api";
import type { TauriGitWorkspaceClient } from "../features/git/api";
import { helperConnectionKey } from "../features/shell/helperUpgrade";
import { hostProfileId } from "../features/shell/types";
import { useDesktopResumeRecovery } from "../features/shell/useDesktopResumeRecovery";
import { TerminalEventHub } from "../features/terminal/TerminalEventHub";
import { createEchoLagProbe } from "../features/terminal/echoLagProbe";
import { createInputLatencyReporter } from "../features/terminal/inputLatencyStats";
import { fetchInputLatencyStats, fetchLinkStats, requestTerminalSeed, startTerminal, stopTerminal, terminalBridgeKey, terminalBridgeScope } from "../features/terminal/api";
import { terminalStateCache } from "../features/terminal/TerminalStateCache";
import { connectionReducer, denormalizeSnapshot, initialHostState } from "../state/connectionReducer";
import type { ConnectionSpec, HostProfile, PersistedProfiles } from "./types";
import { resolveActiveWindowId, type OptimisticWindowSwitch } from "./windowSelection";
import { resolveSelectedSession } from "../features/shell/model";
import type { HostScopeToken } from "../features/shell/hostScope";
import { recordPerfCounter } from "../perf/probe";
import { recordIncident } from "../diagnostics/incidents";
import { writeTerminalApplicationClipboard } from "../features/terminal/terminalTransferApi";

type ControllerArguments = {
  agentClient: TauriAgentClient;
  fileClient: TauriFileWorkspaceClient;
  gitClient: TauriGitWorkspaceClient;
  setStatus: Dispatch<SetStateAction<string>>;
  terminalApplicationClipboardEnabled?: boolean;
  /**
   * Called when an SSH bridge dies with a handshake failure, which is the one
   * connection error that is usually not a connection problem at all: a host
   * with no helper installed answers the exec with "no such file or
   * directory", and the supervisor reports that as a failed handshake. This
   * controller does not know what to do about that — it stays a connection
   * error here, with the detail and the status set exactly as before — so the
   * shell is told and decides whether to offer the install.
   */
  onHandshakeFailure?(connection: ConnectionSpec): void;
};

export function useAppConnectionController({
  agentClient,
  fileClient,
  gitClient,
  setStatus,
  terminalApplicationClipboardEnabled = false,
  onHandshakeFailure,
}: ControllerArguments) {
  // Through a ref, because the bridge effect is keyed on the connection alone:
  // a callback the shell rebuilds every render must not be able to tear the
  // bridge down and start it again.
  const handshakeFailureRef = useRef(onHandshakeFailure);
  handshakeFailureRef.current = onHandshakeFailure;
  const terminalApplicationClipboardEnabledRef = useRef(terminalApplicationClipboardEnabled);
  terminalApplicationClipboardEnabledRef.current = terminalApplicationClipboardEnabled;
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
   * state; Connect is what turns it into a connection.
   *
   * A saved host here is the machine the form is *editing*: Connect keeps this
   * id, so correcting a target moves that host rather than leaving a second
   * entry for the same machine behind it. Empty is the other mode — a host that
   * is not saved yet — which is what "+ Add host" sets, and what changing the
   * transport falls back to, because a saved host does not change transport in
   * place.
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
  /**
   * The journal's record of typing lag, which nothing else can report.
   *
   * One probe for the app: input is dispatched from a single callback and the
   * hub repaints every pane, so both halves of the measurement meet here. The
   * link counters are read only once an outlier has already been decided, and
   * a failed read still leaves the lag itself in the journal.
   */
  /**
   * The distribution behind the outliers, one record per minute of typing.
   *
   * Lives beside the echo probe because it is the same measurement seen whole:
   * the probe's completed round trips are its `endToEnd` segment, the send site
   * and the pane's write callback are the two ends the app can time itself, and
   * the native queue's own histogram is polled from here — where the live
   * client id is — and drained into the same record.
   */
  const inputLatencyReporter = useMemo(() => createInputLatencyReporter({
    recordIncident,
    fetchRustHistogram: () => {
      const currentClientId = clientIdRef.current;
      return currentClientId ? fetchInputLatencyStats(currentClientId) : Promise.resolve(null);
    },
  }), []);
  useEffect(() => () => inputLatencyReporter.dispose(), [inputLatencyReporter]);
  const echoLagProbe = useMemo(() => createEchoLagProbe({
    onSample: (_paneId, lagMs) => inputLatencyReporter.sample("endToEnd", lagMs),
    onIncident: ({ kind, ...detail }) => {
      const currentClientId = clientIdRef.current;
      if (!currentClientId) {
        recordIncident(kind, detail);
        return;
      }
      void fetchLinkStats(currentClientId)
        .then((stats) => recordIncident(kind, stats ? { ...detail, ...stats } : detail));
    },
  }), [inputLatencyReporter]);
  useEffect(() => () => echoLagProbe.dispose(), [echoLagProbe]);
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
      recordIncident("reconnect.observerFailure", { message });
      setConnectionDetail(message);
      setStatus(message);
      setConnectionEpoch((value) => value + 1);
    },
    (paneId) => echoLagProbe.noteOutput(paneId),
  ), [echoLagProbe, setStatus]);

  useDesktopResumeRecovery(() => {
    if (!profilesHydrated) return;
    recordPerfCounter("connection.reconnect.desktopResume");
    recordIncident("reconnect.desktopResume");
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

  /**
   * The last reason left that costs a full rebuild: a different tmux server.
   *
   * Everything else the reducer used to force a reconnect for was sequence
   * bookkeeping, and the native link repairs those on the connection it
   * already holds. A new server identity is not repairable — none of the
   * sessions, windows or panes on screen exist on the machine now answering —
   * so this one still bumps the connection epoch.
   */
  useEffect(() => {
    if (hostState.resyncRequested && !frontendResyncActive.current) {
      frontendResyncActive.current = true;
      recordPerfCounter("connection.reconnect.serverChanged");
      recordIncident("reconnect.serverChanged", { reason: hostState.resyncReason });
      setConnectionEpoch((value) => value + 1);
    } else if (!hostState.resyncRequested) {
      frontendResyncActive.current = false;
    }
  }, [hostState.resyncRequested]);

  /**
   * The journal's record of what the user saw: the amber strip shows exactly
   * while the phase is degraded, so this pair of records is "the strip
   * appeared (and why)" / "it went away after N ms" — the ground truth every
   * amber investigation has been missing.
   */
  const linkDegradedSince = useRef<number | undefined>(undefined);
  /**
   * The pane half of a resync the connection survived.
   *
   * `resyncing` → `connected` is the native link repairing a sequence break in
   * place: ordering is whole again and the connection was never replaced, so
   * nothing else re-establishes the panes — and whatever the missing frames
   * were painting is simply absent from their screens. A connecting → connected
   * transition is an ordinary first attach, whose seeds are already on the way,
   * which is why the previous phase and not the current one decides this.
   */
  const previousPhase = useRef(hostState.phase);
  useEffect(() => {
    const before = previousPhase.current;
    previousPhase.current = hostState.phase;
    if (before !== "resyncing" || hostState.phase !== "connected") return;
    recordIncident("link.resynced", {});
    hub.reseedSubscribedPanes("post-resync reseed");
  }, [hostState.phase, hub]);
  useEffect(() => {
    const degraded = hostState.phase === "disconnected"
      || hostState.phase === "reconnecting"
      || hostState.phase === "resyncing";
    if (degraded && linkDegradedSince.current === undefined) {
      linkDegradedSince.current = Date.now();
      recordIncident("link.degraded", { phase: hostState.phase });
    } else if (!degraded && linkDegradedSince.current !== undefined) {
      recordIncident("link.restored", { afterMs: Date.now() - linkDegradedSince.current });
      linkDegradedSince.current = undefined;
    }
  }, [hostState.phase]);

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
   * A window or workspace switch committed locally and still catching up.
   *
   * Owned here because this is where snapshots decide the active window, and
   * written by `useShellNavigation`, which is the only thing that knows a
   * switch is outstanding. A workspace switch resolves its own window in the
   * same commit and records it here, so the effect below finds nothing left to
   * change; the guard is what keeps the snapshots arriving mid-flight — which
   * still describe the workspace being left — from changing it back. See
   * `OptimisticWindowSwitch`.
   */
  const optimisticWindow = useRef<OptimisticWindowSwitch | undefined>(undefined);
  useEffect(() => {
    const pending = optimisticWindow.current;
    if (pending) {
      // Released once the host has caught up: a snapshot at or past the
      // generation the select action returned is one that has seen the
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
    // The design says dirty→snapshot is instant: the daemon's topology actor
    // wakes on the notification and pushes as soon as tmux answers. The user
    // measures ~5s from `cd` to the Explorer moving, and the tab name — pure
    // snapshot apply, no Explorer machinery — lags identically, so the missing
    // seconds are somewhere in notification→snapshot→apply. These two records
    // decompose that span from the desktop's side; the tmux-side rename time
    // comes from polling the server during a supervised `cd`.
    let topologyDirtyAt: number | undefined;
    let topologyDirtyName: string | undefined;
    const scope = terminalBridgeScope();
    void startTerminal(scope.sessionId, scope.paneIds, connection, (event) => {
      if (disposed) return;
      hub.publish(event, () => {
        if (event.kind === "generationEpoch") {
          terminalStateCache.clear();
          terminalEpochRef.current = event.epoch;
          setTerminalEpoch(event.epoch);
        } else if (event.kind === "topologyDirty") {
          // First dirty of a burst wins: the snapshot that answers a burst
          // answers all of it, and the span worth measuring starts at the
          // notification that started the daemon working.
          if (topologyDirtyAt === undefined) {
            topologyDirtyAt = Date.now();
            topologyDirtyName = event.name;
          }
          recordIncident("topo.dirty", { name: event.name });
          setStatus("Topology changed; reconciling…");
        } else if (event.kind === "flowPaused") {
          // Journal only — the host resumes the pane itself. This is the
          // per-pane mute window tmux opens when the pipeline falls behind,
          // and pairing its timestamps with input.echoTimeout lines is what
          // convicts (or clears) flow control for the typing-lag reports.
          recordIncident("flow.paused", { paneId: event.paneId });
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
            recordIncident("reconnect.flowStall", { paneId: event.paneId });
            setConnectionDetail("A terminal output stream stalled; reconnecting it now.");
            setStatus("Terminal output stalled; reconnecting…");
            setConnectionEpoch((value) => value + 1);
          }
        } else if (event.kind === "clipboardWrite") {
          void writeTerminalApplicationClipboard(
            terminalApplicationClipboardEnabledRef.current,
            event.text,
          ).catch((error) => {
            recordIncident("clipboard.writeFailed", { error: String(error) });
            setStatus(`A terminal application could not write the clipboard: ${String(error)}`);
          });
        } else if (event.kind === "error" || event.kind === "exit") {
          const detail = event.kind === "error" ? event.message : `Detached: ${event.reason}`;
          recordIncident("link.bridgeDown", { kind: event.kind, detail });
          setConnectionDetail(detail);
          setStatus(detail);
          // The message is the only thing that separates "this host has no
          // helper" from "this host cannot be reached": both arrive as a dead
          // bridge, and only the first one has a fix the app can offer. The
          // wording comes from the bridge supervisor, which says either
          // "handshake" or "connection setup" for every failure of that stage.
          if (event.kind === "error" && connection.mode === "ssh" && /handshake|connection setup/i.test(event.message)) {
            handshakeFailureRef.current?.(connection);
          }
        } else if (event.kind === "connectionState") {
          dispatchHost({ type: "connection", phase: event.state });
          if (event.state === "connected") setConnectionDetail("");
          setStatus(event.state === "connected" ? "Live" : `Connection ${event.state}…`);
        } else if (event.kind === "snapshot") {
          if (topologyDirtyAt !== undefined) {
            recordIncident("topo.snapshot", {
              msSinceDirty: Date.now() - topologyDirtyAt,
              answering: topologyDirtyName,
            });
            topologyDirtyAt = undefined;
            topologyDirtyName = undefined;
          }
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
    currentHostScope, dispatchHost, echoLagProbe, hostScopeRef, hostState, hub, inputLatencyReporter,
    profileRecovery,
    optimisticWindow,
    profiles, profilesHydrated, selectedProfileId, setActiveSessionId, setActiveWindowId,
    setConnection, setConnectionDetail, setConnectionEpoch, setConnectionMode,
    setProfileRecovery, setProfiles, setSelectedProfileId, setSshConfigPath, setSshTarget,
    snapshot, snapshotRef, sshConfigPath, sshTarget, terminalEpoch, terminalEpochRef, windows,
  };
}
