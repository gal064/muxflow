import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { AgentClient } from "./api";
import { compareAgentGenerations, generationIsAfter, zeroGeneration } from "./generation";
import { emitNativeAgentNotification, decideAgentNotification } from "./notifications";
import { agentsForScope, agentsMatchingFocusedPane, deriveAgentRollups } from "./selectors";
import { agentReducer, initialAgentState, initialHostAgentState, wireEventHost, type AgentAction } from "./state";
import { playAgentSound, type SoundInstrumentation } from "./sound";
import { AgentRuntimeMemory } from "./runtimeMemory";
import { agentHostIdentity } from "./types";
import type {
  AgentAdapterId,
  AgentFocus,
  AgentHookReview,
  AgentHostNamingOutcome,
  AgentHostProjection,
  AgentLaunchRequest,
  AgentNativeNotification,
  AgentNotificationInstrumentation,
  AgentRecord,
  AgentRequestScope,
  AgentRuntimeScope,
  AgentSoundPreferences,
  AgentTopologyAuthority,
  AgentWireEvent,
  HostAgentState,
} from "./types";

export interface AgentRuntimeOptions {
  client: AgentClient;
  /**
   * One per shown host with a live, mutable client. Compared by content, never
   * by identity: the integrator rebuilds the array whenever any host renders.
   */
  scopes: readonly AgentRuntimeScope[];
  /** Every host with a bridge, scope or not. A host outside this list loses its records. */
  shownHostIds: readonly string[];
  /** The active host; the terminal, seen-acks and every action belong to it. */
  focus: AgentFocus;
  soundPreferences: AgentSoundPreferences;
  onStatus(message: string): void;
  effects?: {
    emitNotification?(notification: AgentNativeNotification): Promise<{ id: number; actionable: boolean }>;
    playSound?(event: "blocked" | "completed", preferences: AgentSoundPreferences, instrument: (event: SoundInstrumentation) => void): Promise<void>;
  };
  onNotificationInstrumentation?(event: AgentNotificationInstrumentation): void;
  onSoundInstrumentation?(event: SoundInstrumentation): void;
}

interface SnapshotRequest {
  client: AgentClient;
  key: string;
  cancel(): void;
}

export function useAgentRuntime(options: AgentRuntimeOptions) {
  const [state, setState] = useState(initialAgentState);
  const [topologyAuthorities, setTopologyAuthorities] = useState<Readonly<Record<string, AgentTopologyAuthority>>>({});
  const stateRef = useRef(state);
  // The wire stream can deliver several events before React renders. Reduce
  // once against the authoritative synchronous ref, then commit that exact
  // state. Only this path writes the ref: a render React later discards must
  // not roll it back to the older state that render began with. Asking a
  // React reducer to repeat the action made every multi-host event rebuild its
  // host slice a second time (and `accept` had already done a third reduction
  // to inspect the transition).
  const apply = useCallback((action: AgentAction) => {
    const previousState = stateRef.current;
    const nextState = agentReducer(previousState, action);
    if (nextState === previousState) return undefined;
    stateRef.current = nextState;
    setState(nextState);
    return { previousState, nextState };
  }, []);
  const optionsRef = useRef(options);
  optionsRef.current = options;
  const runtimeMemory = useRef(new AgentRuntimeMemory());
  const pendingSeen = useRef(new Set<string>());
  const requests = useRef(new Map<string, SnapshotRequest>());
  const projections = useRef(new Map<string, { slice: HostAgentState | undefined; projection: AgentHostProjection }>());

  const processTransition = useCallback((previous: AgentRecord | undefined, next: AgentRecord, replayed: boolean, reconciledSnapshot = false) => {
    const key = recordKey(next);
    const decision = decideAgentNotification(previous, next, {
      focus: optionsRef.current.focus,
      replayed,
      reconciledSnapshot,
      previouslyNotifiedGeneration: runtimeMemory.current.processedGeneration(key),
    });
    if (decision.kind === "ignore") return;
    runtimeMemory.current.markProcessed(key, next.attentionGeneration);
    if (decision.kind !== "emit") {
      optionsRef.current.onNotificationInstrumentation?.(decision.instrumentation);
      return;
    }
    const current = optionsRef.current;
    const play = current.effects?.playSound ?? playAgentSound;
    const emit = current.effects?.emitNotification ?? emitNativeAgentNotification;
    void play(decision.notification.event, current.soundPreferences, (event) => current.onSoundInstrumentation?.(event));
    void emit(decision.notification).then((receipt) => {
      optionsRef.current.onNotificationInstrumentation?.({ ...decision.instrumentation, actionable: receipt.actionable });
      if (!receipt.actionable) optionsRef.current.onStatus("Native notification shown without click actions; in-app attention remains available.");
    }).catch((error) => {
      // Delivery is attempted exactly once. Persistent in-app attention remains.
      optionsRef.current.onNotificationInstrumentation?.({ ...decision.instrumentation, outcome: "failed", error: String(error) });
      optionsRef.current.onStatus(`Native notification unavailable; in-app attention remains: ${String(error)}`);
    });
  }, []);

  const accept = useCallback((
    event: AgentWireEvent,
    coverage?: { scope: AgentRequestScope; windowIds: readonly string[] },
  ) => {
    const hostProfileId = wireEventHost(event);
    // A straggler from a bridge that is being torn down must not resurrect
    // the slice the request effect just dropped.
    if (!optionsRef.current.shownHostIds.includes(hostProfileId)) return;
    const applied = apply({ type: "wire", event });
    if (!applied) return;
    const { previousState, nextState } = applied;
    const previousHost = previousState.byHost[hostProfileId];
    const nextHost = nextState.byHost[hostProfileId];
    if (event.kind === "snapshot") {
      // Only the request effect can pair this agent snapshot with an exact
      // tmux topology. Push snapshots carry no window-set metadata, so they
      // may refresh records but never manufacture an absence proof from the
      // options of whatever render happened to receive them.
      if (coverage
        && event.snapshot.hostProfileId === coverage.scope.hostProfileId
        && event.snapshot.serverIdentity === coverage.scope.serverIdentity
        && event.snapshot.connectionEpoch === coverage.scope.connectionEpoch) {
        setTopologyAuthorities((current) => ({
          ...current,
          [hostProfileId]: {
            hostProfileId: coverage.scope.hostProfileId,
            serverIdentity: coverage.scope.serverIdentity,
            connectionEpoch: coverage.scope.connectionEpoch,
            topologyGeneration: coverage.scope.topologyGeneration,
            coveredWindowIds: new Set(coverage.windowIds),
          },
        }));
      }
      const key = scopeKey(event.snapshot.hostProfileId, event.snapshot.serverIdentity);
      const previousScope = runtimeMemory.current.scope(key);
      const previousRecords = previousScope?.records;
      const previousWatermark = previousScope?.watermark;
      if (previousWatermark === undefined) {
        for (const record of event.snapshot.agents) runtimeMemory.current.markProcessed(recordKey(record), record.attentionGeneration);
      } else if (generationIsAfter(event.snapshot.notificationWatermark, previousWatermark)) {
        for (const record of event.snapshot.agents) processTransition(previousRecords?.[record.id], record, false, true);
      }
      const watermark = previousWatermark === undefined || compareAgentGenerations(event.snapshot.notificationWatermark, previousWatermark) >= 0
        ? event.snapshot.notificationWatermark : previousWatermark;
      runtimeMemory.current.commitScope(key, watermark, nextHost.byId);
      return;
    }
    const key = scopeKey(event.hostProfileId, event.serverIdentity);
    const previousScope = runtimeMemory.current.scope(key);
    const previousRecords = previousScope?.records ?? previousHost?.byId ?? {};
    if (event.kind === "upsert") processTransition(previousRecords[event.record.id], event.record, Boolean(event.replayed));
    runtimeMemory.current.commitScope(key, previousScope?.watermark ?? zeroGeneration, nextHost.byId);
  }, [apply, processTransition]);

  useEffect(() => options.client.subscribe(accept), [accept, options.client]);

  // Anything that changes what the host would answer without changing the
  // scope — installing hooks is the one that exists — bumps this to ask again.
  const [resnapshot, setResnapshot] = useState<Readonly<Record<string, number>>>({});
  const refreshSnapshot = useCallback((hostProfileId = optionsRef.current.focus.hostProfileId) => setResnapshot((current) => ({
    ...current, [hostProfileId]: (current[hostProfileId] ?? 0) + 1,
  })), []);

  const scopeKeys = options.scopes.map((entry) => requestScopeKey(entry.scope)).join("\n");
  const shownKey = options.shownHostIds.join("\n");
  const scopes = useMemo(() => options.scopes, [scopeKeys]);

  useEffect(() => {
    const { client, scopes, shownHostIds } = optionsRef.current;
    const wanted = scopes.filter(({ scope }) => shownHostIds.includes(scope.hostProfileId));
    const inFlight = requests.current;
    const requestKeys = new Map(wanted.map(({ scope }) => [scope.hostProfileId, `${requestScopeKey(scope)}\0${resnapshot[scope.hostProfileId] ?? 0}`]));
    for (const [hostProfileId, request] of inFlight) {
      if (request.client === client && requestKeys.get(hostProfileId) === request.key) continue;
      request.cancel();
      inFlight.delete(hostProfileId);
      if (!requestKeys.has(hostProfileId) && shownHostIds.includes(hostProfileId)) {
        apply({ type: "disconnect", hostProfileId });
        setTopologyAuthorities((current) => without(current, hostProfileId));
      }
    }
    for (const hostProfileId of Object.keys(stateRef.current.byHost)) {
      if (shownHostIds.includes(hostProfileId)) continue;
      apply({ type: "remove", hostProfileId });
      setTopologyAuthorities((current) => without(current, hostProfileId));
    }
    for (const { scope, topologyWindowIds } of wanted) {
      if (inFlight.has(scope.hostProfileId)) continue;
      let cancelled = false;
      inFlight.set(scope.hostProfileId, { client, key: requestKeys.get(scope.hostProfileId)!, cancel: () => { cancelled = true; } });
      void client.snapshot(scope).then((snapshot) => {
        if (!cancelled) accept(
          { kind: "snapshot", snapshot, replayed: true },
          { scope, windowIds: topologyWindowIds },
        );
      }).catch((error) => {
        if (!cancelled) optionsRef.current.onStatus(`Agent snapshot unavailable for ${scope.hostProfileId}: ${String(error)}`);
      });
    }
  }, [accept, apply, options.client, resnapshot, scopeKeys, shownKey]);
  useEffect(() => () => {
    for (const request of requests.current.values()) request.cancel();
    requests.current.clear();
  }, []);

  const focusScope = useMemo(
    () => scopes.find((entry) => entry.scope.hostProfileId === options.focus.hostProfileId)?.scope,
    [options.focus.hostProfileId, scopes],
  );
  const focusHost = state.byHost[options.focus.hostProfileId];
  const projection = useMemo(() => {
    // Mutated during render on purpose: a discarded render can only evict
    // entries, never serve a stale one, because each is keyed by its slice.
    const cache = projections.current;
    const used = new Set<string>();
    const project = (hostProfileId: string, serverIdentity: string | undefined): AgentHostProjection => {
      const slice = state.byHost[hostProfileId];
      const key = `${hostProfileId}\0${serverIdentity}`;
      used.add(key);
      const cached = cache.get(key);
      if (cached && cached.slice === slice) return cached.projection;
      const agents = agentsForScope(slice, hostProfileId, serverIdentity);
      const projection = { agents, adapters: slice?.adapters ?? initialHostAgentState.adapters, rollups: deriveAgentRollups(agents) };
      cache.set(key, { slice, projection });
      return projection;
    };
    const byHost = new Map(scopes.map(({ scope }) => [scope.hostProfileId, project(scope.hostProfileId, scope.serverIdentity)]));
    const active = project(options.focus.hostProfileId, options.focus.serverIdentity);
    for (const key of cache.keys()) if (!used.has(key)) cache.delete(key);
    return { byHost, active };
  }, [options.focus.hostProfileId, options.focus.serverIdentity, scopes, state]);
  const agents = projection.active.agents;

  useEffect(() => {
    const scope = focusScope;
    if (!scope || !options.focus.appFocused || !options.focus.terminalVisible || !options.focus.automaticSeen) return;
    for (const current of agentsMatchingFocusedPane(agents, options.focus.paneId)) {
      const key = [
        scope.hostProfileId,
        scope.serverIdentity,
        scope.connectionEpoch,
        current.agentId,
        current.attentionGeneration,
      ].join("\0");
      if (pendingSeen.current.has(key)) continue;
      pendingSeen.current.add(key);
      void options.client.markSeen(scope, current.agentId, current.attentionGeneration).then((accepted) => {
        apply({
          type: "seenAck",
          ...current,
          attentionSeenAt: accepted?.attentionSeenAt ?? Date.now(),
          hostProfileId: scope.hostProfileId,
          serverIdentity: scope.serverIdentity,
          connectionEpoch: scope.connectionEpoch,
        });
      }).catch((error) => options.onStatus(`Could not mark agent attention seen: ${String(error)}`)).finally(() => {
        pendingSeen.current.delete(key);
      });
    }
  }, [agents, apply, focusScope, options.client, options.focus.appFocused, options.focus.automaticSeen, options.focus.paneId, options.focus.terminalVisible]);

  const launch = useCallback((request: AgentLaunchRequest) => {
    const scope = activeScope(optionsRef.current);
    if (!scope) return Promise.reject(new Error("Agent launch requires a live authoritative host."));
    return optionsRef.current.client.launch(scope, request);
  }, []);
  // A rename is the one row mutation that reaches a peer: it names the agent
  // where it lives. Launch and resume place a new pane, and the only place
  // known well enough for that — an active root under a live pane — is on
  // the host on screen.
  const rename = useCallback((agent: AgentRecord, displayName: string) => {
    const scope = agentScope(optionsRef.current, agent);
    if (!scope) return Promise.reject(new Error("Agent rename requires a live authoritative host."));
    return optionsRef.current.client.rename(scope, agent.id, displayName);
  }, []);
  const resume = useCallback((agent: AgentRecord, request: AgentLaunchRequest) => {
    const scope = activeScope(optionsRef.current);
    if (!scope) return Promise.reject(new Error("Agent resume requires a live authoritative host."));
    return optionsRef.current.client.resume(scope, agent.id, agent.nativeSessionId, request);
  }, []);
  const reviewHooks = useCallback((adapter: AgentAdapterId, action: "install" | "uninstall", expectedHost: string): Promise<AgentHookReview> => {
    const scope = consentedScope(activeScope(optionsRef.current), expectedHost, "Hook review");
    if (scope instanceof Error) return Promise.reject(scope);
    return optionsRef.current.client.reviewHooks(scope, adapter, action).then((review) => ({
      ...review,
      adapterDisplayName: stateRef.current.byHost[scope.hostProfileId]?.adapters.find((descriptor) => descriptor.id === adapter)?.displayName ?? adapter,
    }));
  }, []);
  const applyHooks = useCallback((review: AgentHookReview, expectedHost: string): Promise<void> => {
    const scope = consentedScope(activeScope(optionsRef.current), expectedHost, "Hook installation");
    if (scope instanceof Error) return Promise.reject(scope);
    return optionsRef.current.client.applyHooks(scope, review);
  }, []);
  // The naming changes the tmux server's memory rather than a configuration
  // file, so it is outside the consent invariant — but it is part of the same
  // one-time answer, and an answer about one host must not reach another.
  const applyHostNaming = useCallback((expectedHost: string): Promise<AgentHostNamingOutcome> => {
    const scope = consentedScope(activeScope(optionsRef.current), expectedHost, "Host naming");
    if (scope instanceof Error) return Promise.reject(scope);
    return optionsRef.current.client.applyHostNaming(scope);
  }, []);
  const removeHostNaming = useCallback((expectedHost: string): Promise<AgentHostNamingOutcome> => {
    const scope = consentedScope(activeScope(optionsRef.current), expectedHost, "Host naming");
    if (scope instanceof Error) return Promise.reject(scope);
    return optionsRef.current.client.applyHostNaming(scope, "uninstall");
  }, []);

  return {
    state: focusHost ?? initialHostAgentState,
    topologyAuthority: topologyAuthorities[options.focus.hostProfileId],
    agents, adapters: projection.active.adapters, rollups: projection.active.rollups,
    byHost: projection.byHost as ReadonlyMap<string, AgentHostProjection>,
    accept,
    launch, resume, rename, reviewHooks, applyHooks, applyHostNaming, removeHostNaming, refreshSnapshot,
  };
}

export type AgentRuntime = ReturnType<typeof useAgentRuntime>;

function activeScope(options: AgentRuntimeOptions): AgentRequestScope | undefined {
  return options.scopes.find((entry) => entry.scope.hostProfileId === options.focus.hostProfileId)?.scope;
}

/** The live scope of the host an agent was reported from, active or shown beside it. */
function agentScope(options: AgentRuntimeOptions, agent: AgentRecord): AgentRequestScope | undefined {
  return options.scopes.find((entry) => entry.scope.hostProfileId === agent.hostProfileId)?.scope;
}

/**
 * The scope a hook request may use, or why it may not have one.
 *
 * Every path that writes an agent's configuration file is acting on an answer
 * the user gave about a *named* host, so it passes the identity it was
 * answering for and the request is refused — never redirected — if this app is
 * on another host by the time it runs. M13-E004: the decision, the reviewed
 * diff and the write each resolved "the current host" independently, so a host
 * switch between them wrote a machine the user had never been asked about.
 *
 * `expectedHost` is required rather than optional: an optional guard is one a
 * future caller can opt out of by saying nothing, which is exactly the
 * behaviour this replaced.
 */
function consentedScope(
  scope: AgentRequestScope | undefined,
  expectedHost: string,
  action: string,
): AgentRequestScope | Error {
  if (!scope) return new Error(`${action} requires a live authoritative host.`);
  if (agentHostIdentity(scope) !== expectedHost) {
    return new Error(`${action} was answered for a different host than this app is connected to now; nothing was changed.`);
  }
  return scope;
}

function requestScopeKey(scope: AgentRequestScope): string {
  return [scope.hostProfileId, scope.clientId, scope.connectionEpoch, scope.serverIdentity, scope.topologyGeneration].join("\0");
}

function without<Value>(record: Readonly<Record<string, Value>>, key: string): Readonly<Record<string, Value>> {
  if (!(key in record)) return record;
  const next = { ...record };
  delete next[key];
  return next;
}

function scopeKey(hostProfileId: string, serverIdentity: string): string {
  return `${hostProfileId}\0${serverIdentity}`;
}

function recordKey(record: AgentRecord): string {
  return `${scopeKey(record.hostProfileId, record.serverIdentity)}\0${record.id}`;
}
