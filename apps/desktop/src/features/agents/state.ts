import { agentGeneration, compareAgentGenerations, generationAtLeast, zeroGeneration, type AgentGeneration } from "./generation";
import type { AgentRecord, AgentStoreState, AgentWireEvent, HostAgentState } from "./types";

export const initialHostAgentState: HostAgentState = {
  snapshotRevision: zeroGeneration,
  eventSequence: zeroGeneration,
  authoritative: false,
  byId: {},
  adapters: [],
};

export const initialAgentState: AgentStoreState = { byHost: {} };

export type AgentAction =
  | { type: "wire"; event: AgentWireEvent }
  | {
    type: "seenAck";
    agentId: string;
    attentionGeneration: AgentGeneration;
    attentionSeenAt: number;
    hostProfileId: string;
    serverIdentity: string;
    connectionEpoch: number;
  }
  | { type: "disconnect"; hostProfileId: string }
  /** The host is no longer shown: its slice goes, not just its authority. */
  | { type: "remove"; hostProfileId: string }
  | { type: "reset" };

export function agentReducer(state: AgentStoreState, action: AgentAction): AgentStoreState {
  if (action.type === "reset") return initialAgentState;
  const hostProfileId = action.type === "wire" ? wireEventHost(action.event) : action.hostProfileId;
  if (action.type === "remove") {
    if (!(hostProfileId in state.byHost)) return state;
    const byHost = { ...state.byHost };
    delete byHost[hostProfileId];
    return { byHost };
  }
  const host = state.byHost[hostProfileId] ?? initialHostAgentState;
  const next = hostReducer(host, action);
  return next === host ? state : { byHost: { ...state.byHost, [hostProfileId]: next } };
}

export function wireEventHost(event: AgentWireEvent): string {
  return event.kind === "snapshot" ? event.snapshot.hostProfileId : event.hostProfileId;
}

function hostReducer(state: HostAgentState, action: Exclude<AgentAction, { type: "reset" | "remove" }>): HostAgentState {
  if (action.type === "disconnect") return state.authoritative ? { ...state, authoritative: false } : state;
  if (action.type === "seenAck") {
    if (state.hostProfileId !== action.hostProfileId
      || state.serverIdentity !== action.serverIdentity
      || state.connectionEpoch !== action.connectionEpoch) return state;
    return acknowledgeSeen(
      state,
      action.agentId,
      action.attentionGeneration,
      action.attentionSeenAt,
    );
  }
  const event = action.event;
  if (event.kind === "snapshot") {
    const snapshot = event.snapshot;
    if (sameScope(state, snapshot.hostProfileId, snapshot.serverIdentity)
      && (compareAgentGenerations(snapshot.revision, state.snapshotRevision) < 0
        || compareAgentGenerations(snapshot.acceptedGeneration, state.eventSequence) < 0)) return state;
    const byId: Record<string, AgentRecord> = {};
    for (const record of snapshot.agents) {
      if (validRecord(record, snapshot.hostProfileId, snapshot.serverIdentity)) byId[record.id] = record;
    }
    return {
      hostProfileId: snapshot.hostProfileId,
      serverIdentity: snapshot.serverIdentity,
      connectionEpoch: snapshot.connectionEpoch,
      snapshotRevision: snapshot.revision,
      eventSequence: snapshot.eventSequence,
      authoritative: true,
      byId,
      adapters: snapshot.adapters,
    };
  }
  if (!sameScope(state, event.hostProfileId, event.serverIdentity)
    || event.connectionEpoch !== state.connectionEpoch
    || !state.authoritative || compareAgentGenerations(event.sequence, state.eventSequence) <= 0) return state;
  if (event.kind === "removed") {
    if (!(event.agentId in state.byId)) return { ...state, eventSequence: event.sequence };
    const byId = { ...state.byId };
    delete byId[event.agentId];
    return { ...state, eventSequence: event.sequence, byId };
  }
  if (event.kind === "retired") {
    const present = event.retiredAgentIds.filter((agentId) => agentId in state.byId);
    if (present.length === 0) return { ...state, eventSequence: event.sequence };
    const byId = { ...state.byId };
    for (const agentId of present) delete byId[agentId];
    return { ...state, eventSequence: event.sequence, byId };
  }
  if (!validRecord(event.record, event.hostProfileId, event.serverIdentity)) {
    return { ...state, eventSequence: event.sequence };
  }
  const previous = state.byId[event.record.id];
  if (previous && compareAgentGenerations(event.record.lifecycleGeneration, previous.lifecycleGeneration) < 0) {
    return { ...state, eventSequence: event.sequence };
  }
  const byId = { ...state.byId };
  for (const retiredId of event.retiredAgentIds ?? []) delete byId[retiredId];
  byId[event.record.id] = event.record;
  return { ...state, eventSequence: event.sequence, byId };
}

function sameScope(state: HostAgentState, hostProfileId: string, serverIdentity: string): boolean {
  return state.hostProfileId === undefined
    || (state.hostProfileId === hostProfileId && state.serverIdentity === serverIdentity);
}

function validRecord(record: AgentRecord, hostProfileId: string, serverIdentity: string): boolean {
  const validRoute = !record.paneId || Boolean(record.windowId && record.sessionId);
  return record.hostProfileId === hostProfileId
    && record.serverIdentity === serverIdentity
    && Boolean(record.id) && validRoute
    && validGeneration(record.lifecycleGeneration)
    && validGeneration(record.attentionGeneration)
    && validGeneration(record.seenGeneration);
}

function validGeneration(value: AgentGeneration): boolean {
  try { return agentGeneration(value) === value; } catch { return false; }
}

/** Optimistically mirrors only the exact generation acknowledged by the host. */
export function acknowledgeSeen(
  state: HostAgentState,
  agentId: string,
  attentionGeneration: AgentGeneration,
  attentionSeenAt = Date.now(),
): HostAgentState {
  const record = state.byId[agentId];
  if (!record || record.attentionGeneration !== attentionGeneration) return state;
  const advances = !generationAtLeast(record.seenGeneration, attentionGeneration);
  const suppliesMissingTimestamp = attentionSeenAt > 0 && record.attentionSeenAt <= 0;
  if (!advances && !suppliesMissingTimestamp) return state;
  return {
    ...state,
    byId: {
      ...state.byId,
      [agentId]: {
        ...record,
        seenGeneration: advances ? attentionGeneration : record.seenGeneration,
        attentionSeenAt: suppliesMissingTimestamp ? attentionSeenAt : record.attentionSeenAt,
      },
    },
  };
}
