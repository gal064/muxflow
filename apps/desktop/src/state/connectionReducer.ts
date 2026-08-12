import type { TmuxSnapshot } from "../app/types";

export type ConnectionPhase =
  | "disconnected"
  | "connecting"
  | "reconnecting"
  | "resyncing"
  | "connected"
  | "readOnly";

export interface NormalizedHostState {
  phase: ConnectionPhase;
  canMutate: boolean;
  serverIdentity?: string;
  generation: number;
  lastSequence: number;
  resyncRequested: boolean;
  sessions: Record<string, TmuxSnapshot["sessions"][number]>;
  windows: Record<string, TmuxSnapshot["windows"][number]>;
  panes: Record<string, TmuxSnapshot["panes"][number]>;
}

export type HostAction =
  | { type: "connection"; phase: ConnectionPhase }
  | { type: "snapshot"; snapshot: TmuxSnapshot; sequence: number; serverIdentity: string; generation?: number }
  | { type: "orderedSnapshot"; snapshot: TmuxSnapshot; sequence: number; serverIdentity: string; generation?: number }
  | { type: "orderedEvent"; sequence: number }
  | { type: "sequenceGap"; expected: number; received: number }
  | { type: "reset" };

export const initialHostState: NormalizedHostState = {
  phase: "disconnected",
  canMutate: false,
  generation: 0,
  lastSequence: 0,
  resyncRequested: false,
  sessions: {},
  windows: {},
  panes: {},
};

function indexById<T extends { id: string }>(items: T[]): Record<string, T> {
  return Object.fromEntries(items.map((item) => [item.id, item]));
}

function replaceSnapshot(
  state: NormalizedHostState,
  action: Extract<HostAction, { type: "snapshot" | "orderedSnapshot" }>,
): NormalizedHostState {
  return {
    ...state,
    serverIdentity: action.serverIdentity,
    generation: action.generation ?? state.generation + 1,
    lastSequence: action.sequence,
    resyncRequested: false,
    sessions: indexById(action.snapshot.sessions),
    windows: indexById(action.snapshot.windows),
    panes: indexById(action.snapshot.panes),
  };
}

export function connectionReducer(state: NormalizedHostState, action: HostAction): NormalizedHostState {
  switch (action.type) {
    case "reset":
      return initialHostState;
    case "connection":
      return {
        ...state,
        phase: action.phase,
        canMutate: action.phase === "connected",
      };
    case "snapshot":
      return replaceSnapshot(state, action);
    case "orderedEvent": {
      if (action.sequence <= state.lastSequence) return state;
      if (action.sequence !== state.lastSequence + 1) {
        return {
          ...state,
          phase: "resyncing",
          canMutate: false,
          resyncRequested: true,
        };
      }
      return { ...state, lastSequence: action.sequence };
    }
    case "sequenceGap":
      return {
        ...state,
        phase: "resyncing",
        canMutate: false,
        resyncRequested: true,
      };
    case "orderedSnapshot": {
      if (action.serverIdentity !== state.serverIdentity) {
        return {
          ...state,
          phase: "resyncing",
          canMutate: false,
          resyncRequested: true,
        };
      }
      if (action.sequence <= state.lastSequence) return state;
      if (action.sequence !== state.lastSequence + 1) {
        return {
          ...state,
          phase: "resyncing",
          canMutate: false,
          resyncRequested: true,
        };
      }
      if (action.generation !== undefined && action.generation <= state.generation) {
        return {
          ...state,
          phase: "resyncing",
          canMutate: false,
          resyncRequested: true,
        };
      }
      return replaceSnapshot(state, action);
    }
  }
}

export function denormalizeSnapshot(state: NormalizedHostState): TmuxSnapshot {
  return {
    sessions: Object.values(state.sessions),
    windows: Object.values(state.windows),
    panes: Object.values(state.panes),
  };
}
