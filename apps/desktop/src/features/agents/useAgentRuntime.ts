import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import type { AgentClient } from "./api";
import { compareAgentGenerations, generationIsAfter, zeroGeneration } from "./generation";
import { emitNativeAgentNotification, decideAgentNotification } from "./notifications";
import { agentsForScope, agentsMatchingFocusedPane, deriveAgentRollups } from "./selectors";
import { agentReducer, initialAgentState } from "./state";
import { playAgentSound, type SoundInstrumentation } from "./sound";
import { AgentRuntimeMemory } from "./runtimeMemory";
import { agentHostIdentity } from "./types";
import type {
  AgentAdapterId,
  AgentFocus,
  AgentHookReview,
  AgentHostNamingOutcome,
  AgentLaunchRequest,
  AgentNativeNotification,
  AgentNotificationInstrumentation,
  AgentRecord,
  AgentRequestScope,
  AgentSoundPreferences,
  AgentTopologyAuthority,
  AgentWireEvent,
} from "./types";

export interface AgentRuntimeOptions {
  client: AgentClient;
  scope?: AgentRequestScope;
  focus: AgentFocus;
  /** Window IDs in the topology whose generation the requested snapshot covers. */
  topologyWindowIds: readonly string[];
  soundPreferences: AgentSoundPreferences;
  onStatus(message: string): void;
  effects?: {
    emitNotification?(notification: AgentNativeNotification): Promise<{ id: number; actionable: boolean }>;
    playSound?(event: "blocked" | "completed", preferences: AgentSoundPreferences, instrument: (event: SoundInstrumentation) => void): Promise<void>;
  };
  onNotificationInstrumentation?(event: AgentNotificationInstrumentation): void;
  onSoundInstrumentation?(event: SoundInstrumentation): void;
}

export function useAgentRuntime(options: AgentRuntimeOptions) {
  const [state, dispatch] = useReducer(agentReducer, initialAgentState);
  const [topologyAuthority, setTopologyAuthority] = useState<AgentTopologyAuthority>();
  const stateRef = useRef(state);
  stateRef.current = state;
  const optionsRef = useRef(options);
  optionsRef.current = options;
  const runtimeMemory = useRef(new AgentRuntimeMemory());
  const pendingSeen = useRef(new Set<string>());

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
    const previousState = stateRef.current;
    const nextState = agentReducer(previousState, { type: "wire", event });
    if (nextState === previousState) return;
    stateRef.current = nextState;
    dispatch({ type: "wire", event });
    if (event.kind === "snapshot") {
      // Only the request effect can pair this agent snapshot with an exact
      // tmux topology. Push snapshots carry no window-set metadata, so they
      // may refresh records but never manufacture an absence proof from the
      // options of whatever render happened to receive them.
      if (coverage
        && event.snapshot.hostProfileId === coverage.scope.hostProfileId
        && event.snapshot.serverIdentity === coverage.scope.serverIdentity
        && event.snapshot.connectionEpoch === coverage.scope.connectionEpoch) {
        setTopologyAuthority({
          hostProfileId: coverage.scope.hostProfileId,
          serverIdentity: coverage.scope.serverIdentity,
          connectionEpoch: coverage.scope.connectionEpoch,
          topologyGeneration: coverage.scope.topologyGeneration,
          coveredWindowIds: new Set(coverage.windowIds),
        });
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
      runtimeMemory.current.commitScope(key, watermark, nextState.byId);
      return;
    }
    const key = scopeKey(event.hostProfileId, event.serverIdentity);
    const previousScope = runtimeMemory.current.scope(key);
    const previousRecords = previousScope?.records ?? previousState.byId;
    if (event.kind === "upsert") processTransition(previousRecords[event.record.id], event.record, Boolean(event.replayed));
    runtimeMemory.current.commitScope(key, previousScope?.watermark ?? zeroGeneration, nextState.byId);
  }, [processTransition]);

  useEffect(() => options.client.subscribe(accept), [accept, options.client]);

  // Anything that changes what the host would answer without changing the
  // scope — installing hooks is the one that exists — bumps this to ask again.
  const [resnapshot, setResnapshot] = useState(0);
  const refreshSnapshot = useCallback(() => setResnapshot((value) => value + 1), []);

  useEffect(() => {
    if (!options.scope) {
      dispatch({ type: "disconnect" });
      setTopologyAuthority(undefined);
      return;
    }
    let cancelled = false;
    const captured = options.scope;
    const capturedWindowIds = options.topologyWindowIds;
    void options.client.snapshot(captured).then((snapshot) => {
      if (!cancelled) accept(
        { kind: "snapshot", snapshot, replayed: true },
        { scope: captured, windowIds: capturedWindowIds },
      );
    }).catch((error) => {
      if (!cancelled) options.onStatus(`Agent snapshot unavailable: ${String(error)}`);
    });
    return () => { cancelled = true; };
  }, [accept, options.client, options.scope?.clientId, options.scope?.connectionEpoch, options.scope?.hostProfileId, options.scope?.serverIdentity, options.scope?.topologyGeneration, resnapshot]);

  const agents = useMemo(
    () => agentsForScope(state, options.focus.hostProfileId, options.focus.serverIdentity),
    [options.focus.hostProfileId, options.focus.serverIdentity, state],
  );
  const rollups = useMemo(() => deriveAgentRollups(agents), [agents]);

  useEffect(() => {
    const scope = options.scope;
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
        dispatch({
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
  }, [agents, options.client, options.focus.appFocused, options.focus.automaticSeen, options.focus.paneId, options.focus.terminalVisible, options.scope]);

  const launch = useCallback((request: AgentLaunchRequest) => {
    if (!optionsRef.current.scope) return Promise.reject(new Error("Agent launch requires a live authoritative host."));
    return optionsRef.current.client.launch(optionsRef.current.scope, request);
  }, []);
  const rename = useCallback((agent: AgentRecord, displayName: string) => {
    if (!optionsRef.current.scope) return Promise.reject(new Error("Agent rename requires a live authoritative host."));
    return optionsRef.current.client.rename(optionsRef.current.scope, agent.id, displayName);
  }, []);
  const resume = useCallback((agent: AgentRecord, request: AgentLaunchRequest) => {
    if (!optionsRef.current.scope) return Promise.reject(new Error("Agent resume requires a live authoritative host."));
    return optionsRef.current.client.resume(optionsRef.current.scope, agent.id, agent.nativeSessionId, request);
  }, []);
  const reviewHooks = useCallback((adapter: AgentAdapterId, action: "install" | "uninstall", expectedHost: string): Promise<AgentHookReview> => {
    const scope = consentedScope(optionsRef.current.scope, expectedHost, "Hook review");
    if (scope instanceof Error) return Promise.reject(scope);
    return optionsRef.current.client.reviewHooks(scope, adapter, action).then((review) => ({
      ...review,
      adapterDisplayName: stateRef.current.adapters.find((descriptor) => descriptor.id === adapter)?.displayName ?? adapter,
    }));
  }, []);
  const applyHooks = useCallback((review: AgentHookReview, expectedHost: string): Promise<void> => {
    const scope = consentedScope(optionsRef.current.scope, expectedHost, "Hook installation");
    if (scope instanceof Error) return Promise.reject(scope);
    return optionsRef.current.client.applyHooks(scope, review);
  }, []);
  // The naming changes the tmux server's memory rather than a configuration
  // file, so it is outside the consent invariant — but it is part of the same
  // one-time answer, and an answer about one host must not reach another.
  const applyHostNaming = useCallback((expectedHost: string): Promise<AgentHostNamingOutcome> => {
    const scope = consentedScope(optionsRef.current.scope, expectedHost, "Host naming");
    if (scope instanceof Error) return Promise.reject(scope);
    return optionsRef.current.client.applyHostNaming(scope);
  }, []);
  const removeHostNaming = useCallback((expectedHost: string): Promise<AgentHostNamingOutcome> => {
    const scope = consentedScope(optionsRef.current.scope, expectedHost, "Host naming");
    if (scope instanceof Error) return Promise.reject(scope);
    return optionsRef.current.client.applyHostNaming(scope, "uninstall");
  }, []);

  return {
    state, topologyAuthority, agents, adapters: state.adapters, rollups, accept,
    launch, resume, rename, reviewHooks, applyHooks, applyHostNaming, removeHostNaming, refreshSnapshot,
  };
}

export type AgentRuntime = ReturnType<typeof useAgentRuntime>;

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

function scopeKey(hostProfileId: string, serverIdentity: string): string {
  return `${hostProfileId}\0${serverIdentity}`;
}

function recordKey(record: AgentRecord): string {
  return `${scopeKey(record.hostProfileId, record.serverIdentity)}\0${record.id}`;
}
