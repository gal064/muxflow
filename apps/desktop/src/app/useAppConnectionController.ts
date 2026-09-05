import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useMemo, useReducer, useRef, useState, type SetStateAction } from "react";
import type { TauriAgentClient } from "../features/agents/api";
import type { TauriFileWorkspaceClient } from "../features/files/api";
import type { TauriGitWorkspaceClient } from "../features/git/api";
import { helperConnectionKey } from "../features/shell/helperUpgrade";
import { hostProfileId } from "../features/shell/types";
import {
  probeResumedLink,
  useDesktopResumeRecovery,
  type ResumeProbeOutcome,
} from "../features/shell/useDesktopResumeRecovery";
import { TerminalEventHub } from "../features/terminal/TerminalEventHub";
import { createEchoLagProbe } from "../features/terminal/echoLagProbe";
import { createInputLatencyReporter } from "../features/terminal/inputLatencyStats";
import {
  fetchInputLatencyStats,
  fetchLinkStats,
  prewarmTerminalBulk,
  requestTerminalSeed,
  selectTerminalSession,
  startTerminal,
  stopTerminal,
  terminalBridgeKey,
  terminalBridgeScope,
  type TerminalEvent,
} from "../features/terminal/api";
import { terminalCacheKey, terminalStateCache } from "../features/terminal/TerminalStateCache";
import { denormalizeSnapshot, type ConnectionPhase, type HostAction } from "../state/connectionReducer";
import {
  emptyHostLink,
  hostLinkScope,
  hostLinksReducer,
  initialHostLinksState,
  shownHostProfiles,
  syncHostLinks,
  type HostLink,
} from "../state/hostLinks";
import { userFacingBridgeFailure } from "./bridgeFailureText";
import {
  createLinkQualityMonitor,
  describeLinkQuality,
  LINK_QUALITY_POLL_MS,
  LINK_STATS_POLL_MS,
  type LinkQualityChange,
} from "./linkQuality";
import type { ConnectionSpec, HostProfile, PersistedProfiles } from "./types";
import { resolveActiveWindowId, type OptimisticWindowSwitch } from "./windowSelection";
import type { HostScopeToken } from "../features/shell/hostScope";
import { perfProbeEnabled, recordPerfCounter, recordPerfRecord } from "../perf/probe";
import { recordIncident } from "../diagnostics/incidents";
import { writeTerminalApplicationClipboard } from "../features/terminal/terminalTransferApi";

type ControllerArguments = {
  agentClient: TauriAgentClient;
  fileClient: TauriFileWorkspaceClient;
  gitClient: TauriGitWorkspaceClient;
  setStatus: (status: string) => void;
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
  /** The bridge transport changed state; helper digest reconciliation may react. */
  onConnectionStateChanged?(
    connection: ConnectionSpec,
    state: Extract<TerminalEvent, { kind: "connectionState" }>["state"],
  ): void;
};

/** One native bridge, from `startTerminal` to `stopTerminal`. */
interface RunningBridge {
  /** `terminalBridgeKey` of the link it was started for; a different key is a different bridge. */
  key: string;
  clientId?: string;
  /** The native GenerationEpoch, known the instant it arrives rather than one render later. */
  terminalEpoch: number;
  stop(): void;
}

/**
 * What a host keeps across its bridges: the hub its panes subscribe to, the
 * tmux server it last heard from, and the per-host bookkeeping of the effects
 * that used to hold one ref each for the one host there was.
 */
interface LinkRuntime {
  hub: TerminalEventHub;
  bridge?: RunningBridge;
  /**
   * The identity of the tmux server this host last answered as. Outlives the
   * bridge on purpose: a reconnect that finds a different server behind the
   * same address must forget every screen kept for the old one.
   */
  serverIdentity?: string;
  /** The phase the transition effect last saw for this host. */
  phase: ConnectionPhase;
  degradedSince?: number;
  resyncActive: boolean;
}

export function useAppConnectionController({
  agentClient,
  fileClient,
  gitClient,
  setStatus,
  terminalApplicationClipboardEnabled = false,
  onHandshakeFailure,
  onConnectionStateChanged,
}: ControllerArguments) {
  // Through a ref, because the bridge effect is keyed on the link set alone:
  // a callback the shell rebuilds every render must not be able to tear a
  // bridge down and start it again.
  const handshakeFailureRef = useRef(onHandshakeFailure);
  handshakeFailureRef.current = onHandshakeFailure;
  const connectionStateChangedRef = useRef(onConnectionStateChanged);
  connectionStateChangedRef.current = onConnectionStateChanged;
  const terminalApplicationClipboardEnabledRef = useRef(terminalApplicationClipboardEnabled);
  terminalApplicationClipboardEnabledRef.current = terminalApplicationClipboardEnabled;
  /**
   * Every shown host, each with its own bridge, reducer state and remembered
   * session. The active host is a pointer into this — `connection` below —
   * and the fields this hook returns singly are that link's.
   */
  const [links, dispatchLinks] = useReducer(hostLinksReducer, initialHostLinksState);
  // Read by bridge callbacks and the imperative facade, none of which may be
  // rebuilt per render: a bridge must not restart because a link changed.
  const linksRef = useRef(links);
  linksRef.current = links;
  const [connection, setConnectionState] = useState<ConnectionSpec>({ mode: "local" });
  const [connectionMode, setConnectionMode] = useState<"local" | "ssh">("local");
  const [sshTarget, setSshTarget] = useState("");
  const [sshConfigPath, setSshConfigPath] = useState("");
  const [profiles, setProfiles] = useState<HostProfile[]>([]);
  const profilesRef = useRef(profiles);
  profilesRef.current = profiles;
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
  const [appFocused, setAppFocused] = useState(() => typeof document === "undefined" || document.hasFocus());
  const activeProfileId = hostProfileId(connection);
  /**
   * Which host the facade's setters aim at, moved the moment `setConnection`
   * is called rather than one render later: a reset or an epoch bump issued
   * after it in the same tick lands on the host just chosen, not the one
   * being left. Only the pointer moves early; `clientIdRef` and the epoch
   * ref follow with the commit, as they always have.
   */
  const activeProfileIdRef = useRef(activeProfileId);
  activeProfileIdRef.current = activeProfileId;
  const connectionRef = useRef(connection);
  connectionRef.current = connection;
  const setConnection = useCallback((update: SetStateAction<ConnectionSpec>) => {
    const next = typeof update === "function" ? update(connectionRef.current) : update;
    connectionRef.current = next;
    activeProfileIdRef.current = hostProfileId(next);
    setConnectionState(next);
  }, []);
  // A host that is pointed at before its link exists — the render between
  // choosing a new host and the link set catching up — reads as a host that
  // has said nothing yet, which is exactly what it is.
  const placeholderLink = useMemo(() => emptyHostLink(activeProfileId, connection, 0), [activeProfileId, connection]);
  const activeLink = links.byProfileId[activeProfileId] ?? placeholderLink;
  const {
    activeSessionId, activeWindowId, clientId, connectionEpoch, detail: connectionDetail, hostState, terminalEpoch,
  } = activeLink;
  const snapshot = useMemo(() => denormalizeSnapshot(hostState), [hostState]);
  const snapshotRef = useRef(snapshot);
  snapshotRef.current = snapshot;
  // Read by the resume handler, which may not be keyed on the selection.
  const activeSessionIdRef = useRef(activeSessionId);
  activeSessionIdRef.current = activeSessionId;
  const hostPhaseRef = useRef(hostState.phase);
  hostPhaseRef.current = hostState.phase;
  /**
   * The active host's live client, for everything that acts on "the current
   * host" from outside a render. Set from the link's state each render, and
   * the instant a bridge learns its id — before that state commits — as the
   * single-host controller always did.
   */
  const clientIdRef = useRef<string | undefined>(undefined);
  clientIdRef.current = clientId;
  /**
   * Which host profile `clientId` was established for. A link's client is by
   * construction its own host's, so this is the active host whenever there is
   * a client at all — kept because the agent controller still asks.
   */
  const clientHostProfileId = clientId === undefined ? undefined : activeProfileId;
  const currentHostScope: HostScopeToken = hostLinkScope(activeLink);
  const hostScopeRef = useRef(currentHostScope);
  hostScopeRef.current = currentHostScope;
  /**
   * The one thing six drops in two minutes never told the user: it is the
   * network.
   *
   * The signals it tallies already exist here — a bridge error arriving while
   * the link was up, the echo probe's outliers, and the native late-request
   * counter — so this is glue around `linkQuality`, not a new measurement.
   * One monitor, for the active host: it is the link the user is typing over.
   */
  const linkQuality = useMemo(() => createLinkQualityMonitor(), []);
  // The standing verdict, while an episode lasts. It is spoken once, as a
  // notice (dismissible, and out of the way of the tabs, the host row and
  // the tmux status line — every fixed row a standing strip was found to
  // cover), and it stands in for the raw bridge failure in the reconnecting
  // strip's detail line for as long as the episode lasts.
  const [linkQualityVerdict, setLinkQualityVerdict] = useState("");
  const linkQualityVerdictRef = useRef("");
  linkQualityVerdictRef.current = linkQualityVerdict;
  // Empty for a local connection: there is no network to blame there, and the
  // monitor is not fed at all.
  const linkQualityHostRef = useRef("");
  linkQualityHostRef.current = connection.mode === "local" ? "" : connection.target;
  /** Returns whether a verdict was just spoken, so the caller can hold its own notice. */
  const applyLinkQuality = useCallback((change: LinkQualityChange | undefined): boolean => {
    if (!change) return false;
    if (change.kind === "degraded") {
      recordIncident("link.quality", {
        state: change.state,
        losses: change.losses,
        lagEvents: change.lagEvents,
        lateRequests: change.lateRequests,
      });
      const verdict = describeLinkQuality(change.state, linkQualityHostRef.current);
      linkQualityVerdictRef.current = verdict;
      setLinkQualityVerdict(verdict);
      setStatus(verdict);
      return true;
    } else {
      recordIncident("link.quality", { state: "ok", afterMs: change.afterMs });
      linkQualityVerdictRef.current = "";
      setLinkQualityVerdict("");
    }
    return false;
  }, [setStatus]);
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
  /**
   * The journal's record of typing lag, which nothing else can report.
   *
   * One probe for the app: input is dispatched from a single callback and the
   * active host's hub repaints every pane, so both halves of the measurement
   * meet here. The link counters are read only once an outlier has already
   * been decided, and a failed read still leaves the lag itself in the journal.
   */
  const echoLagProbe = useMemo(() => createEchoLagProbe({
    onSample: (_paneId, lagMs) => inputLatencyReporter.sample("endToEnd", lagMs),
    // Only a measured process pays for this: with the probe off there is no
    // reading to take, the probe skips its second call, and no record is built.
    sampleLinkCounters: () => {
      const currentClientId = clientIdRef.current;
      if (!currentClientId || !perfProbeEnabled()) return undefined;
      return fetchLinkStats(currentClientId).then((stats) =>
        stats?.bytesReadTotal === undefined || stats.framesReadTotal === undefined
          ? undefined
          : { bytesRead: stats.bytesReadTotal, framesRead: stats.framesReadTotal });
    },
    // The fast baseline the outlier lines have to be compared against. It goes
    // to the perf log rather than the journal: `input.echoLag` is the incident,
    // this is the measurement.
    onEcho: ({ paneId, sentAt, echoAt, lagMs, inputCount, bytesAhead, framesAhead, outputSequence, outputGeneration, connectionEpoch }) =>
      recordPerfRecord("perf.echo", {
        paneId, sentAt, echoAt, lagMs, inputCount, bytesAhead, framesAhead,
        outputSequence, outputGeneration, connectionEpoch,
      }),
    onIncident: ({ kind, ...detail }) => {
      // The probe has already applied its own threshold and its own per-pane
      // dedupe, so an outlier here is exactly one occasion of "the host
      // answered late" — no second measurement and no timer of our own.
      if (kind === "input.echoLag" && "lagMs" in detail && linkQualityHostRef.current) {
        applyLinkQuality(linkQuality.noteEchoLag(Date.now(), detail.lagMs));
      }
      const currentClientId = clientIdRef.current;
      if (!currentClientId) {
        recordIncident(kind, detail);
        return;
      }
      void fetchLinkStats(currentClientId)
        .then((stats) => recordIncident(kind, stats ? { ...detail, ...stats } : detail));
    },
  }), [applyLinkQuality, inputLatencyReporter, linkQuality]);
  useEffect(() => () => echoLagProbe.dispose(), [echoLagProbe]);

  /**
   * Per-host runtime, keyed by profile id. Created the first time a host is
   * asked about — which can be a render, for the facade's `hub`: the surface
   * needs the hub as a prop, and get-or-create is idempotent, so a render
   * React discards leaves nothing behind but the runtime the next one wants —
   * and dropped when its link stops.
   */
  const runtimes = useRef(new Map<string, LinkRuntime>());
  const runtimeFor = useCallback((profileId: string): LinkRuntime => {
    let runtime = runtimes.current.get(profileId);
    if (runtime) return runtime;
    const hub = new TerminalEventHub(
      (paneId, reason) => {
        // The hub outlives the bridge it was built under; a reseed is for the
        // pane on this host's *live* client. The cached screen is this host's
        // — pane ids repeat across hosts — and the seed request must still go
        // out: the hub asks once per pane and waits for the answer.
        terminalStateCache.delete(terminalCacheKey(profileId, paneId));
        const currentClientId = runtimes.current.get(profileId)?.bridge?.clientId;
        if (!currentClientId) return;
        void requestTerminalSeed(currentClientId, paneId).catch((error) => {
          setStatus(`${reason}; seed request failed: ${String(error)}`);
        });
      },
      {},
      undefined,
      (message) => {
        recordPerfCounter("connection.reconnect.observerFailure");
        recordIncident("reconnect.observerFailure", { hostProfileId: profileId, message });
        dispatchLinks({ type: "detail", profileId, detail: message });
        if (profileId === activeProfileIdRef.current) setStatus(message);
        dispatchLinks({ type: "reconnect", profileId });
      },
      // The probe is keyed by pane id, and pane ids repeat across hosts: only
      // the host the user is typing on may close its round trips.
      (paneId, sequence, generation, connectionEpoch) => {
        if (profileId === activeProfileIdRef.current) {
          echoLagProbe.noteOutput(paneId, { sequence, generation, connectionEpoch });
        }
      },
    );
    runtime = { hub, phase: "disconnected", resyncActive: false };
    runtimes.current.set(profileId, runtime);
    return runtime;
  }, [echoLagProbe, setStatus]);
  const hub = runtimeFor(activeProfileId).hub;

  /**
   * A resume rebuilds the connections only once the link it has fails to answer.
   *
   * The native wake notification fires for every full wake, including the ones
   * a Power Nap dark wake already reconnected for and the short sleeps that
   * never dropped the link. Rebuilding on each of those replaced a healthy
   * connection — snapshot, reseed, the host attaching its first session — with
   * the toast and the workspace jump the user reported. So the active host's
   * link, if connected, is probed with one correlated request first
   * (`RESUME_PROBE_TIMEOUT_MS`), and only a link that does not answer costs a
   * rebuild — of every host, because they all slept through the same suspend.
   * A link that is not connected has nothing to probe and rebuilds as it
   * always did.
   */
  useDesktopResumeRecovery((trigger) => {
    if (!profilesHydrated) return;
    const rebuild = (probe: ResumeProbeOutcome | "skipped") => {
      recordPerfCounter("connection.reconnect.desktopResume");
      recordIncident("reconnect.desktopResume", { trigger, probe });
      for (const profileId of linksRef.current.order) {
        dispatchLinks({ type: "detail", profileId, detail: "System resumed; reconnecting for an authoritative state refresh." });
      }
      setStatus("System resumed; reconnecting…");
      dispatchLinks({ type: "reconnectAll" });
    };
    const probeClientId = clientIdRef.current;
    const probeSessionId = activeSessionIdRef.current;
    if (hostPhaseRef.current !== "connected" || !probeClientId || !probeSessionId) {
      rebuild("skipped");
      return;
    }
    const probedProfileId = activeProfileIdRef.current;
    const probedBridge = runtimes.current.get(probedProfileId)?.bridge;
    const probedEpoch = probedBridge?.terminalEpoch;
    void probeResumedLink(() => selectTerminalSession(probeClientId, probeSessionId)).then((outcome) => {
      // The bridge that was probed is the one the outcome speaks for. A link
      // the native supervisor replaced meanwhile carries a new epoch, and one
      // the renderer restarted is a new bridge: either is already the
      // authoritative rebuild this would have asked for. A host switch in the
      // meantime is neither — the probed link is still the one that slept.
      const bridge = runtimes.current.get(probedProfileId)?.bridge;
      if (bridge !== probedBridge || bridge?.terminalEpoch !== probedEpoch) return;
      if (outcome === "alive") {
        recordPerfCounter("connection.resume.linkAlive");
        recordIncident("resume.linkAlive", { trigger });
        return;
      }
      rebuild(outcome);
    });
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
   * What a host's phase change means, for every host.
   *
   * Three facts per link, each of which used to be one ref for the one host:
   *
   * - A different tmux server is the last reason left that costs a full
   *   rebuild. Everything else the reducer used to force a reconnect for was
   *   sequence bookkeeping, which the native link repairs on the connection
   *   it already holds. A new server identity is not repairable — none of the
   *   sessions, windows or panes on screen exist on the machine now answering
   *   — so it still bumps that host's connection epoch, and only that host's.
   * - `resyncing` → `connected` is the native link repairing a sequence break
   *   in place: ordering is whole again and the connection was never
   *   replaced, so nothing else re-establishes the panes — and whatever the
   *   missing frames were painting is simply absent from their screens. A
   *   connecting → connected transition is an ordinary first attach, whose
   *   seeds are already on the way, which is why the previous phase and not
   *   the current one decides this.
   * - The journal's record of what the user saw: the amber strip shows exactly
   *   while the active phase is degraded, so this pair of records is "the
   *   strip appeared (and why)" / "it went away after N ms" — the ground truth
   *   every amber investigation has been missing.
   */
  useEffect(() => {
    for (const profileId of links.order) {
      const link = links.byProfileId[profileId];
      const runtime = runtimeFor(profileId);
      const { phase, resyncRequested } = link.hostState;
      if (resyncRequested && !runtime.resyncActive) {
        runtime.resyncActive = true;
        recordPerfCounter("connection.reconnect.serverChanged");
        recordIncident("reconnect.serverChanged", { hostProfileId: profileId, reason: link.hostState.resyncReason });
        dispatchLinks({ type: "reconnect", profileId });
      } else if (!resyncRequested) {
        runtime.resyncActive = false;
      }
      const before = runtime.phase;
      runtime.phase = phase;
      if (before === "resyncing" && phase === "connected") {
        recordIncident("link.resynced", { hostProfileId: profileId });
        runtime.hub.reseedSubscribedPanes("post-resync reseed");
      }
      const degraded = phase === "disconnected" || phase === "reconnecting" || phase === "resyncing";
      if (degraded && runtime.degradedSince === undefined) {
        runtime.degradedSince = Date.now();
        recordIncident("link.degraded", { hostProfileId: profileId, phase });
      } else if (!degraded && runtime.degradedSince !== undefined) {
        recordIncident("link.restored", { hostProfileId: profileId, afterMs: Date.now() - runtime.degradedSince });
        runtime.degradedSince = undefined;
      }
    }
  }, [links, runtimeFor]);

  /**
   * Late requests are the slow link itself — a host answer that missed its
   * deadline — and the native side counts them (it cannot send an event from
   * a request thread: the delivery ledger belongs to the bridge thread). Read
   * on a slow cadence while the active link is up; each increment is one late
   * request.
   */
  useEffect(() => {
    if (hostState.phase !== "connected" || !clientId || !linkQualityHostRef.current) return;
    let seen: number | undefined;
    const timer = setInterval(() => {
      void fetchLinkStats(clientId).then((stats) => {
        if (!stats) return;
        if (seen !== undefined && stats.lateRequestsTotal > seen) {
          applyLinkQuality(linkQuality.noteLateRequest(Date.now()));
        }
        seen = stats.lateRequestsTotal;
      });
    }, LINK_STATS_POLL_MS);
    return () => clearInterval(timer);
  }, [applyLinkQuality, clientId, hostState.phase, linkQuality]);

  /**
   * File transfer is a second helper connection. Warm it only for the host on
   * screen: shown peers keep their topology and agent bridges, but do not spend
   * an idle bulk-pool slot or evict the active host's. Native deduplicates this
   * by terminal epoch, so returning to a host is free and an active reconnect
   * earns exactly one new warmup.
   */
  useEffect(() => {
    if (hostState.phase !== "connected" || !hostState.canMutate || !clientId || terminalEpoch === 0) return;
    void prewarmTerminalBulk(clientId).catch(() => undefined);
  }, [clientId, hostState.canMutate, hostState.phase, terminalEpoch]);

  /** Only time ends an episode, and only an episode pays for the timer. */
  useEffect(() => {
    if (!linkQualityVerdict) return;
    const timer = setInterval(() => applyLinkQuality(linkQuality.poll(Date.now())), LINK_QUALITY_POLL_MS);
    return () => clearInterval(timer);
  }, [applyLinkQuality, linkQuality, linkQualityVerdict]);

  /** A different machine's link is a different link; nothing carries over — not a verdict, and not a keystroke still waiting for its echo. */
  useEffect(() => {
    linkQuality.reset();
    echoLagProbe.reset();
    linkQualityVerdictRef.current = "";
    setLinkQualityVerdict("");
  }, [activeProfileId, echoLagProbe, linkQuality]);

  useEffect(() => {
    void invoke<PersistedProfiles>("list_host_profiles").then((saved) => {
      setProfiles(saved.profiles);
      setProfileRecovery(saved.recovery);
      const selected = saved.profiles.find((profile) => profile.id === saved.lastProfileId);
      if (!selected) return;
      setConnection(selected.connection);
      setConnectionMode(selected.connection.mode);
      setSelectedProfileId(selected.id);
      if (selected.connection.mode === "ssh") {
        setSshTarget(selected.connection.target);
        setSshConfigPath(selected.connection.configPath ?? "");
      }
    }).catch((error) => setStatus(String(error))).finally(() => setProfilesHydrated(true));
  }, [setConnection, setStatus]);

  /**
   * The link set follows the saved hosts: every checked profile, and the
   * active host whether checked or not. Nothing before the profiles are
   * loaded — the saved host must be the first bridge, not a Local one that
   * is torn down the moment the list arrives.
   */
  const shownProfiles = useMemo(
    () => (profilesHydrated ? shownHostProfiles(profiles, connection) : []),
    [connection, profiles, profilesHydrated],
  );
  // Reconciled during render rather than in an effect, so the link set and
  // the pointer agree in the same commit. Left to an effect, the corrected
  // address and the epoch bump Connect sends together would commit once with
  // the new epoch on the old address, and the bridge effect would open a
  // bridge to the address the user had just left, only to replace it a render
  // later. React re-runs the render at once when the reducer returns a
  // different state, and the reducer returns the same one when nothing
  // changed, so this settles in one pass.
  const syncedLinks = useMemo(() => syncHostLinks(links, shownProfiles), [links, shownProfiles]);
  if (syncedLinks !== links) dispatchLinks({ type: "sync", hosts: shownProfiles });

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
    dispatchLinks({
      type: "activeWindow",
      profileId: activeProfileId,
      windowId: (current) => resolveActiveWindowId(windows, current, preferred),
    });
  }, [activeProfileId, activeSessionId, hostState.generation, snapshot.windows]);

  /**
   * One host's bridge, from start to stop: every event it reports, handled
   * the same way for every host, with the host's profile id deciding which
   * link the answer belongs to and whether it is the one on screen.
   */
  function startLinkBridge(link: HostLink, runtime: LinkRuntime): RunningBridge {
    const { profileId, connection: linkConnection } = link;
    const isActive = () => profileId === activeProfileIdRef.current;
    const linkPhase = () => linksRef.current.byProfileId[profileId]?.hostState.phase;
    /** Status the shell shows for the host on screen; a host beside it keeps that to its own rows. */
    const announce = (message: string) => { if (isActive()) setStatus(message); };
    const setDetail = (detail: string) => dispatchLinks({ type: "detail", profileId, detail });
    const dispatchHost = (action: HostAction) => dispatchLinks({ type: "host", profileId, action });
    // Only the host on screen attaches a terminal. A host shown beside it
    // relays topology and agents until it is activated and a session is
    // selected on it — and its bridge is never restarted to get there.
    const attach = isActive();
    let disposed = false;
    let recoveringFlowStall = false;
    /**
     * The last bridge failure already shown as a notice, for as long as the
     * link stays down.
     *
     * The supervisor reports every failed reconnect attempt, so one overnight
     * outage is seven or eight copies of the same sentence climbing the backoff
     * ladder, and a connection problem is a notice the shell never
     * auto-dismisses — the user wakes to a stack of identical errors. The
     * disconnected strip carries the standing state; the notice only has to
     * say what changed. Cleared when the transport reports itself connected
     * again, and a new bridge starts from silence — the Reconnect button
     * restarts the bridge without ever passing through `connected`, and a
     * deliberate press that fails the same way still owes the user an answer.
     */
    let lastBridgeFailure: string | undefined;
    // The design says dirty→snapshot is instant: the daemon's topology actor
    // wakes on the notification and pushes as soon as tmux answers. The user
    // measures ~5s from `cd` to the Explorer moving, and the tab name — pure
    // snapshot apply, no Explorer machinery — lags identically, so the missing
    // seconds are somewhere in notification→snapshot→apply. One `topo.snapshot`
    // line per answered burst decomposes that span from the desktop's side: the
    // span itself, the name from the notification that started the burst, and
    // how many dirty notifications the burst contained. The per-event dirty
    // lines were folded in here because they tripled the journal without adding
    // a fact this record does not already carry. The tmux-side rename time comes
    // from polling the server during a supervised `cd`.
    let topologyDirtyAt: number | undefined;
    let topologyDirtyName: string | undefined;
    let topologyDirtyCount = 0;
    const bridge: RunningBridge = {
      key: terminalBridgeKey(linkConnection, link.connectionEpoch),
      terminalEpoch: 0,
      stop() {
        disposed = true;
        if (clientIdRef.current === bridge.clientId) clientIdRef.current = undefined;
        dispatchHost({ type: "connection", phase: "disconnected" });
        dispatchLinks({ type: "client", profileId, clientId: undefined });
        dispatchLinks({ type: "terminalEpoch", profileId, terminalEpoch: 0 });
        if (bridge.clientId === undefined) return;
        void stopTerminal(bridge.clientId);
        // The file client outlives any one bridge, so a connection that has
        // gone must take its shared directory watches with it: the host lost
        // those registrations along with the connection, and the records left
        // behind hold promises nothing can settle.
        fileClient.retireConnection(bridge.clientId);
      },
    };
    const scope = terminalBridgeScope(attach ? link.activeSessionId : undefined);
    void startTerminal(scope.sessionId, scope.paneIds, linkConnection, attach, (event) => {
      if (disposed) return;
      runtime.hub.publish(event, () => {
        if (event.kind === "generationEpoch") {
          terminalStateCache.clearScope(profileId);
          bridge.terminalEpoch = event.epoch;
          dispatchLinks({ type: "terminalEpoch", profileId, terminalEpoch: event.epoch });
        } else if (event.kind === "topologyDirty") {
          // First dirty of a burst wins: the snapshot that answers a burst
          // answers all of it, and the span worth measuring starts at the
          // notification that started the daemon working.
          if (topologyDirtyAt === undefined) {
            topologyDirtyAt = Date.now();
            topologyDirtyName = event.name;
          }
          topologyDirtyCount += 1;
          announce("Topology changed; reconciling…");
        } else if (event.kind === "flowPaused") {
          // Journal only — the host resumes the pane itself. This is the
          // per-pane mute window tmux opens when the pipeline falls behind,
          // and pairing its timestamps with input.echoTimeout lines is what
          // convicts (or clears) flow control for the typing-lag reports.
          recordIncident("flow.paused", { paneId: event.paneId });
        } else if (event.kind === "flowStalled") {
          terminalStateCache.delete(terminalCacheKey(profileId, event.paneId));
          // The host already tried the only in-place tmux resume twice. Its
          // fallback seed can repaint the last screen, but cannot restart the
          // stream after that budget is exhausted — exactly the pane that
          // appears frozen until a tab remount changes its attachment. Replace
          // the attachment authoritatively instead. One bridge may report more
          // than one affected pane, so collapse all of them into one reconnect.
          if (!recoveringFlowStall) {
            recoveringFlowStall = true;
            recordPerfCounter("connection.reconnect.terminalFlowStall");
            recordIncident("reconnect.flowStall", { hostProfileId: profileId, paneId: event.paneId });
            setDetail("A terminal output stream stalled; reconnecting it now.");
            announce("Terminal output stalled; reconnecting…");
            dispatchLinks({ type: "reconnect", profileId });
          }
        } else if (event.kind === "clipboardWrite") {
          // A host that was active once stays attached, so it can still send
          // these; only the host on screen may write the user's clipboard.
          if (!isActive()) return;
          void writeTerminalApplicationClipboard(
            terminalApplicationClipboardEnabledRef.current,
            event.text,
          ).catch((error) => {
            recordIncident("clipboard.writeFailed", { error: String(error) });
            setStatus(`A terminal application could not write the clipboard: ${String(error)}`);
          });
        } else if (event.kind === "error" || event.kind === "exit") {
          const detail = event.kind === "error" ? event.message : `Detached: ${event.reason}`;
          recordIncident("link.bridgeDown", { hostProfileId: profileId, event: event.kind, detail });
          const shown = userFacingBridgeFailure(detail);
          const wasConnected = linkPhase() === "connected";
          // An error while the link is up is the link dropping under the app —
          // exactly one per outage, whatever the backoff ladder reports after
          // it, and none for the restarts the app orders itself (a resume, a
          // flow-stall recovery), which arrive without one. The quality
          // monitor watches the link the user types over, so only the active
          // host feeds it. A verdict spoken here is the notice for this drop;
          // the raw failure would only be a second line saying less.
          const verdictSpoken = event.kind === "error"
            && wasConnected
            && isActive()
            && Boolean(linkQualityHostRef.current)
            && applyLinkQuality(linkQuality.noteLinkLost(Date.now()));
          // Under a link that keeps dropping the reconnecting strip is on
          // screen most of the time, and its detail line is where the verdict
          // is worth more than the reader's symptom. Only a bridge failure is
          // replaced: helper guidance and the rest keep their own words.
          setDetail((isActive() && linkQualityVerdictRef.current) || shown);
          // Every attempt is journalled and every attempt stands in the strip;
          // only a failure the user has not already been told about is worth a
          // notice. While the link is up this is the first failure of an
          // outage, which always speaks — for the host on screen. A host
          // beside it shows its failure as its own dot and detail.
          const repeated = !wasConnected && shown === lastBridgeFailure;
          lastBridgeFailure = shown;
          if (!repeated && !verdictSpoken) announce(shown);
          // The message is the only thing that separates "this host has no
          // helper" from "this host cannot be reached": both arrive as a dead
          // bridge, and only the first one has a fix the app can offer. The
          // wording comes from the bridge supervisor, which says either
          // "handshake" or "connection setup" for every failure of that stage.
          // Helper recovery is the active host's: it probes the connection on
          // screen and offers the install there. A host beside it keeps its
          // failure in its own detail until it is activated.
          if (event.kind === "error" && isActive() && linkConnection.mode === "ssh" && /handshake|connection setup/i.test(event.message)) {
            handshakeFailureRef.current?.(linkConnection);
          }
        } else if (event.kind === "connectionState") {
          if (event.state === "resyncing") {
            recordIncident("link.resyncStarted", {
              hostProfileId: profileId,
              reason: event.detail ?? "resync reason unavailable",
            });
          }
          // Let helper reconciliation observe the active host's transport
          // transition before publishing it to the shell. React batches both
          // updates, so a live or read-only helper probe can arbitrate against
          // other one-time host questions on that first settled render without
          // delaying the bridge.
          if (isActive()) connectionStateChangedRef.current?.(linkConnection, event.state);
          dispatchHost({ type: "connection", phase: event.state });
          if (event.state === "connected") {
            setDetail("");
            lastBridgeFailure = undefined;
          }
          announce(event.state === "connected" ? "Live" : `Connection ${event.state}…`);
        } else if (event.kind === "snapshot") {
          if (topologyDirtyAt !== undefined) {
            recordIncident("topo.snapshot", {
              hostProfileId: profileId,
              msSinceDirty: Date.now() - topologyDirtyAt,
              answering: topologyDirtyName,
              dirtyCount: topologyDirtyCount,
            });
            topologyDirtyAt = undefined;
            topologyDirtyName = undefined;
            topologyDirtyCount = 0;
          }
          if (runtime.serverIdentity !== undefined && runtime.serverIdentity !== event.serverIdentity) {
            terminalStateCache.clearScope(profileId);
            runtime.hub.clearTerminalState();
          }
          runtime.serverIdentity = event.serverIdentity;
          // The link's remembered session follows the snapshot in the same
          // reduction — see the reducer's `host` action.
          dispatchHost({ type: "snapshot", snapshot: event.snapshot, sequence: event.sequence, generation: event.generation, serverIdentity: event.serverIdentity });
          // An event with no snapshot is the host's reconciliation
          // acknowledgement: the notification burst is answered and the world
          // is the one already on screen. Either way it closes the reconciling
          // status above.
          announce("Live");
        } else if (event.kind === "fileService" || event.kind === "gitService") {
          // The file and git clients are the active host's: the explorer,
          // the diffs and the watches on screen belong to it, and a root
          // change from a host that was active once — still attached, still
          // relaying — would move the explorer onto a machine the user is
          // not looking at. A host beside the active one relays topology and
          // agents only.
          if (!isActive()) return;
          if (event.kind === "fileService") fileClient.publishWireEvent(event.event);
          else gitClient.publishWireEvent(event.event);
        } else if (event.kind === "agentService") {
          const serverIdentity = runtime.serverIdentity;
          if (!serverIdentity) return;
          const agentScope = {
            clientId: bridge.clientId ?? "",
            hostProfileId: profileId,
            serverIdentity,
            topologyGeneration: linksRef.current.byProfileId[profileId]?.hostState.generation ?? 0,
            connectionEpoch: bridge.terminalEpoch,
          };
          if (event.snapshot) agentClient.publishWireSnapshot(agentScope, event.snapshot);
          else if (event.event) agentClient.publishWireEvent(agentScope, event.event);
        }
      });
    }).then((id) => {
      bridge.clientId = id;
      if (disposed) {
        void stopTerminal(id);
        return;
      }
      if (isActive()) clientIdRef.current = id;
      dispatchLinks({ type: "client", profileId, clientId: id });
    }).catch((error) => {
      if (disposed) return;
      setDetail(String(error));
      announce(String(error));
    });
    return bridge;
  }

  // One bridge per link, alive for as long as the link keeps its connection
  // and epoch. Keyed on those facts alone: a link's other state — its
  // snapshot, its client, its remembered session — changes constantly and
  // must never restart a bridge, and neither must a change to a *different*
  // host's link. The diff is against the bridges actually running, so a host
  // that stops being shown loses its bridge and nothing else is touched. The
  // closure is pinned on purpose: `startLinkBridge` reads every changing fact
  // through a ref, and the clients, `setStatus` and the telemetry it captures
  // are built once for the app.
  const bridgeKeys = useMemo(() => links.order
    .map((profileId) => `${profileId}\0${terminalBridgeKey(links.byProfileId[profileId].connection, links.byProfileId[profileId].connectionEpoch)}`)
    .join("\n"), [links]);
  useEffect(() => {
    const wanted = linksRef.current;
    for (const [profileId, runtime] of runtimes.current) {
      const link = wanted.byProfileId[profileId];
      if (runtime.bridge && link && runtime.bridge.key === terminalBridgeKey(link.connection, link.connectionEpoch)) continue;
      runtime.bridge?.stop();
      runtime.bridge = undefined;
      // The facade holds the active host's hub through the render before its
      // link exists; every other host's runtime goes with its link.
      if (!link && profileId !== activeProfileIdRef.current) runtimes.current.delete(profileId);
    }
    for (const profileId of wanted.order) {
      const runtime = runtimeFor(profileId);
      if (!runtime.bridge) runtime.bridge = startLinkBridge(wanted.byProfileId[profileId], runtime);
    }
  }, [bridgeKeys]);
  useEffect(() => () => {
    for (const runtime of runtimes.current.values()) {
      runtime.bridge?.stop();
      runtime.bridge = undefined;
    }
  }, []);

  /**
   * Makes another shown host the one on screen.
   *
   * The pointer moves; the bridges do not. The host's own link keeps the
   * session the user left it on, so that is the one selected on its client —
   * which is also what turns a bridge that has only relayed topology into an
   * attached one. A host with no session known yet is selected by the shell's
   * visible-session assertion once its snapshot arrives. A profile that is not
   * shown becomes shown by being activated; its link appears with the pointer.
   */
  const activateHost = useCallback((profileId: string) => {
    const link = linksRef.current.byProfileId[profileId];
    const target = link?.connection ?? profilesRef.current.find((item) => item.id === profileId)?.connection;
    if (!target) return;
    void invoke("set_last_profile_id", { profileId }).catch((error) => setStatus(String(error)));
    if (profileId === activeProfileIdRef.current) return;
    setConnection(target);
    // Every snapshot lands a link on a session — the remembered one, or the
    // first — so a link with none has not heard from its host yet, and the
    // shell's visible-session assertion selects it when it does. A refusal
    // here is journalled, not shown: that assertion retries the same fact.
    const targetClientId = runtimes.current.get(profileId)?.bridge?.clientId;
    if (targetClientId && link?.activeSessionId) {
      void selectTerminalSession(targetClientId, link.activeSessionId).catch((error) => {
        recordIncident("host.activateSelectFailed", { hostProfileId: profileId, message: String(error) });
      });
    }
  }, [setConnection, setStatus]);
  const reconnectHost = useCallback((profileId: string) => dispatchLinks({ type: "reconnect", profileId }), []);
  const hubFor = useCallback((profileId: string) => runtimeFor(profileId).hub, [runtimeFor]);
  /** The link as of the latest render; for rendering, read `links` instead. */
  const linkFor = useCallback((profileId: string): HostLink | undefined => linksRef.current.byProfileId[profileId], []);

  // The single-host facade, aimed at the active link. Each targets the host
  // that is active when it is called, which after a `setConnection` in the
  // same tick is already the host just chosen.
  const dispatchHost = useCallback((action: HostAction) => {
    dispatchLinks({ type: "host", profileId: activeProfileIdRef.current, action });
  }, []);
  const setActiveSessionId = useCallback((sessionId: SetStateAction<string | undefined>) => {
    dispatchLinks({ type: "activeSession", profileId: activeProfileIdRef.current, sessionId });
  }, []);
  const setActiveWindowId = useCallback((windowId: SetStateAction<string | undefined>) => {
    dispatchLinks({ type: "activeWindow", profileId: activeProfileIdRef.current, windowId });
  }, []);
  const setConnectionDetail = useCallback((update: SetStateAction<string>) => {
    const profileId = activeProfileIdRef.current;
    const detail = typeof update === "function" ? update(linksRef.current.byProfileId[profileId]?.detail ?? "") : update;
    dispatchLinks({ type: "detail", profileId, detail });
  }, []);
  /**
   * Restarts the active host's bridge. Epochs are minted by the links
   * reducer, so the number a caller asks for is not the one assigned; every
   * caller asks for "one more than now", and any request for a change is
   * honoured as exactly that. A host with no link yet — one being connected
   * to for the first time — starts on a fresh bridge anyway, so there is
   * nothing to bump.
   */
  const setConnectionEpoch = useCallback((update: SetStateAction<number>) => {
    const profileId = activeProfileIdRef.current;
    const link = linksRef.current.byProfileId[profileId];
    if (!link) return;
    const requested = typeof update === "function" ? update(link.connectionEpoch) : update;
    if (requested !== link.connectionEpoch) dispatchLinks({ type: "reconnect", profileId });
  }, []);
  const linkList = useMemo<readonly HostLink[]>(
    () => links.order.map((profileId) => links.byProfileId[profileId]),
    [links],
  );

  return {
    activateHost, activeProfileId, activeSessionId, activeWindowId, appFocused, clientHostProfileId, clientId,
    clientIdRef, connection, connectionDetail, connectionEpoch, connectionMode,
    currentHostProfileId: activeProfileId, currentHostScope, dispatchHost, echoLagProbe, hostScopeRef, hostState,
    hub, hubFor, inputLatencyReporter, linkFor, links: linkList,
    profileRecovery,
    optimisticWindow,
    profiles, profilesHydrated, reconnectHost, selectedProfileId, setActiveSessionId, setActiveWindowId,
    setConnection, setConnectionDetail, setConnectionEpoch, setConnectionMode,
    setProfileRecovery, setProfiles, setSelectedProfileId, setSshConfigPath, setSshTarget,
    snapshot, snapshotRef, sshConfigPath, sshTarget, terminalEpoch, windows,
  };
}
