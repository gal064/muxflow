import type { TmuxSnapshot } from "../app/types";
import { agentRoutingFingerprint } from "../features/agents/topologyAuthority";

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
  /** Latest topology generation that changed agent ownership or detection. */
  agentRoutingChangedAtGeneration: number;
  /** Current agent-relevant structure, excluding cosmetic topology fields. */
  agentRoutingFingerprint: string;
  /**
   * Whether `generation` was stamped by the connection that is live now.
   *
   * The host counts topology generations from zero, per process, so the number
   * only means anything inside one connection: a reconnected link — or a
   * different tmux server — legitimately starts over, and the first snapshot it
   * sends is the new baseline whatever it is numbered. Until a snapshot has set
   * that baseline, a lower generation cannot be told from a restarted one, so
   * none is refused.
   */
  generationBaselined: boolean;
  lastSequence: number;
  resyncRequested: boolean;
  /**
   * Which mismatch requested the resync, with the identities that proved it.
   * Written for the incident journal: `resyncRequested` alone says the world
   * is being rebuilt, and this says why — the difference between knowing the
   * amber bar showed and knowing what to fix.
   *
   * Only a different tmux server answering sets this now. Sequence integrity
   * belongs to the native link, which repairs a break in place and keeps the
   * connection; a frontend that rebuilt the world on its own gap bookkeeping
   * was tearing down exactly the link that repair had just fixed.
   */
  resyncReason?: string;
  sessions: Record<string, TmuxSnapshot["sessions"][number]>;
  windows: Record<string, TmuxSnapshot["windows"][number]>;
  panes: Record<string, TmuxSnapshot["panes"][number]>;
}

export type HostAction =
  | { type: "connection"; phase: ConnectionPhase }
  /**
   * A topology answer. Without a `snapshot` it is the host's reconciliation
   * acknowledgement — a notified pass that found the world unchanged — and
   * carries nothing to apply. See `replaceSnapshot`.
   */
  | { type: "snapshot"; snapshot?: TmuxSnapshot; sequence: number; serverIdentity: string; generation?: number }
  | { type: "orderedSnapshot"; snapshot?: TmuxSnapshot; sequence: number; serverIdentity: string; generation?: number }
  | { type: "orderedEvent"; sequence: number }
  | { type: "reset" };

export const initialHostState: NormalizedHostState = {
  phase: "disconnected",
  canMutate: false,
  generation: 0,
  agentRoutingChangedAtGeneration: 0,
  agentRoutingFingerprint: agentRoutingFingerprint({ sessions: [], windows: [], panes: [] }),
  generationBaselined: false,
  lastSequence: 0,
  resyncRequested: false,
  sessions: {},
  windows: {},
  panes: {},
};

function indexById<T extends { id: string }>(items: T[]): Record<string, T> {
  return Object.fromEntries(items.map((item) => [item.id, item]));
}

function requestResync(state: NormalizedHostState, reason: string): NormalizedHostState {
  return {
    ...state,
    phase: "resyncing",
    canMutate: false,
    resyncRequested: true,
    resyncReason: reason,
  };
}

/**
 * A snapshot the transport delivered late, after a newer one for the same tmux
 * server already landed.
 *
 * Applying it would walk `generation` backwards, and every action stamped
 * against a generation the host has already left is refused as stale — until
 * some later snapshot happens to carry the world forward again. The baseline
 * flag is what keeps this from swallowing a restart, which is the one time a
 * lower generation is the truth.
 */
function precedesLiveGeneration(
  state: NormalizedHostState,
  action: Extract<HostAction, { type: "snapshot" | "orderedSnapshot" }>,
): boolean {
  return state.generationBaselined
    && action.serverIdentity === state.serverIdentity
    && action.generation !== undefined
    && action.generation < state.generation;
}

function replaceSnapshot(
  state: NormalizedHostState,
  action: Extract<HostAction, { type: "snapshot" | "orderedSnapshot" }>,
): NormalizedHostState {
  // Nothing to replace. The host reconciled a notification burst, found the
  // world exactly as it had already described it, and said so with the
  // generation alone — it used to resend the whole server to say that, which
  // on a busy tree is tens of kilobytes, several times per window switch, and
  // ahead of the switch's own answer on the same lane. The entities held here
  // are that same world, so the frame spends its sequence and changes nothing
  // else. `resyncRequested` is deliberately untouched: a rebuild this process
  // asked for is answered by a world, never by word that none was needed.
  //
  // The generation is the one thing the acknowledgement does carry, and it is
  // adopted: every action this side sends is stamped against it, and holding a
  // stale number is how a switch is refused for describing a world the host has
  // already left. Forward only — `precedesLiveGeneration` has refused a stale
  // frame before this, and a frame with no world in it is never the baseline
  // that may walk the generation back.
  if (!action.snapshot) {
    const generation = action.generation !== undefined && action.generation > state.generation
      ? action.generation
      : state.generation;
    return { ...state, generation, lastSequence: action.sequence };
  }
  const generation = action.generation ?? state.generation + 1;
  const routingFingerprint = agentRoutingFingerprint(action.snapshot);
  const newGenerationBaseline = !state.generationBaselined
    || action.serverIdentity !== state.serverIdentity;
  return {
    ...state,
    serverIdentity: action.serverIdentity,
    generation,
    agentRoutingChangedAtGeneration: newGenerationBaseline
      || routingFingerprint !== state.agentRoutingFingerprint
      ? generation
      : state.agentRoutingChangedAtGeneration,
    agentRoutingFingerprint: routingFingerprint,
    generationBaselined: true,
    lastSequence: action.sequence,
    resyncRequested: false,
    resyncReason: undefined,
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
        // A transport transition can be a restarted host process behind the
        // same tmux server, counting generations from zero again. Whatever the
        // next snapshot carries is the baseline from here.
        //
        // Only a transition *away* from connected says that, though. The
        // bridge announces a new link as GenerationEpoch, then the
        // authoritative snapshot, and only then ConnectionState{connected} —
        // so clearing the flag on the way in disarmed the guard immediately
        // after the baseline it was meant to protect had landed, and the next
        // late lower-generation snapshot walked the generation backwards.
        generationBaselined: action.phase === "connected" && state.generationBaselined,
      };
    case "snapshot":
      if (precedesLiveGeneration(state, action)) return state;
      return replaceSnapshot(state, action);
    case "orderedEvent": {
      // Stale frames are still dropped; a jump forward is not this layer's to
      // adjudicate. The native link is the sequence authority — it repairs a
      // break on the connection it already holds — so the watermark follows
      // what arrives instead of freezing the world behind it.
      if (action.sequence <= state.lastSequence) return state;
      return { ...state, lastSequence: action.sequence };
    }
    case "orderedSnapshot": {
      // The one mismatch the frontend must still rebuild for: a different tmux
      // server cannot be reconciled with the entities on screen, whatever the
      // sequence numbers say about them.
      if (action.serverIdentity !== state.serverIdentity) {
        return requestResync(state, `snapshot-identity had=${state.serverIdentity} received=${action.serverIdentity}`);
      }
      if (action.sequence <= state.lastSequence) return state;
      if (precedesLiveGeneration(state, action)) return state;
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
