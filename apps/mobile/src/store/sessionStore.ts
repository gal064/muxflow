// Topology + agents + connection state for the connected host (design doc §8).
// One zustand store, single host. Ids are tmux ids verbatim ("$3", "@7", "%12").

import { createStore, type StoreApi } from "zustand/vanilla";
import {
  AgentHookWiring,
  AgentLifecycleState,
  type AgentAdapterDescriptor as ProtoAgentAdapterDescriptor,
  type AgentEvent,
  type AgentRecord as ProtoAgentRecord,
  type AgentSnapshot,
  type Snapshot,
} from "../protocol/gen/envelope_pb";

export type ConnectionState =
  | "idle"
  | "sshConnecting"
  | "awaitingHostKeyTrust"
  | "handshaking"
  | "connected"
  | "reconnecting"
  | "failed"
  | "incompatible";

export interface SavedHostRef {
  id: string;
  label: string;
  host: string;
  port: number;
  user: string;
}

export interface ConnectionSlice {
  state: ConnectionState;
  host?: SavedHostRef | undefined;
  message?: string | undefined;
  /** Which reconnect attempt is in progress (1-based) while `reconnecting`/`sshConnecting`; 0 once connected. */
  attempt: number;
  /** While `reconnecting`: when the next dial is due (epoch ms), for the strip countdown (§9). */
  retryAtMs?: number | undefined;
}

/** `pinned` is host-owned presentation state (pins sidecar), shared by every client. */
export interface Session { id: string; name: string; windowCount: number; order: number; pinned: boolean }
export interface Window { id: string; sessionId: string; index: number; name: string; active: boolean; pinned: boolean }
export interface Pane {
  id: string;
  sessionId: string;
  windowId: string;
  index: number;
  active: boolean;
  width: number;
  height: number;
  /** Cell offset inside the window, so the window's own size can be read off its panes (D6). */
  left: number;
  top: number;
  currentPath: string;
  currentCommand: string;
}

export type AgentLifecycle = "working" | "blocked" | "idle" | "unknown";
export type AgentAttentionKind = "" | "blocked" | "completed";

export interface AgentRoute {
  sessionId: string;
  sessionNameFallback: string;
  windowId: string;
  windowNameFallback: string;
  paneId: string;
  paneIndexFallback: number;
}

export interface Agent {
  id: string;
  adapterId: string;
  displayName: string;
  lifecycle: AgentLifecycle;
  attentionKind: AgentAttentionKind;
  stateGeneration: bigint;
  attentionGeneration: bigint;
  seenGeneration: bigint;
  updatedAtMs: number;
  /**
   * When `lifecycle` last changed on the host. Repeated hooks that report the
   * same lifecycle and route-only updates move `updatedAtMs` but not this, so
   * lists ordered by it do not reshuffle on every hook.
   */
  lifecycleChangedAtMs: number;
  /**
   * When the current attention generation was first acknowledged (zero while
   * unread or without attention). The recency clock for an acknowledged
   * completion: the row stays Recent for a window after the user saw it, not
   * after the agent finished.
   */
  attentionSeenAtMs: number;
  present: boolean;
  route: AgentRoute;
}

export type AgentHookWiringState = "unspecified" | "wired" | "partial" | "notWired" | "unavailable" | "absent";

export interface AgentAdapterDescriptor {
  id: string;
  displayName: string;
  /** §9.3.1: the empty state mentions hook setup when no adapter is wired. */
  hookWiring: AgentHookWiringState;
}

export interface SessionState {
  connection: ConnectionSlice;
  serverIdentity: string;
  topologyGeneration: bigint;
  sessions: Record<string, Session>;
  windows: Record<string, Window>;
  panes: Record<string, Pane>;
  agents: Record<string, Agent>;
  adapters: AgentAdapterDescriptor[];
  notificationWatermark: bigint;
  /** Pane currently displayed on the Terminal screen. */
  focusedPaneId?: string | undefined;
}

export interface AgentTransition {
  prev: Agent | undefined;
  next: Agent;
}

export interface SessionActions {
  setConnection(connection: Partial<ConnectionSlice>): void;
  setServerIdentity(serverIdentity: string): void;
  /** §8.1: replace the three topology maps wholesale; agents too when the embedded snapshot is authoritative. */
  applySnapshot(snapshot: Snapshot): void;
  /**
   * §8.1: a TOPOLOGY_SNAPSHOT that carries no snapshot acknowledges an
   * unchanged world with the generation alone. The entities are kept; only the
   * generation moves, so a guarded request sends the current one.
   */
  applyTopologyAck(generation: bigint): void;
  /** §8.2: an authoritative snapshot replaces the whole agent map and sets the watermark. */
  applyAgentSnapshot(snapshot: AgentSnapshot): AgentTransition[];
  /** §8.2: upsert `event.agent` unless stale; remove `retiredAgentIds`. Returns the transition when an upsert happened. */
  applyAgentEvent(event: AgentEvent): AgentTransition | undefined;
  setFocusedPane(paneId: string | undefined): void;
  /** Forget everything that belongs to a connection (kept: `connection`, `focusedPaneId`). */
  clearHostState(): void;
}

export type SessionStore = StoreApi<SessionState & SessionActions>;

export function initialSessionState(): SessionState {
  return {
    connection: { state: "idle", attempt: 0 },
    serverIdentity: "",
    topologyGeneration: 0n,
    sessions: {},
    windows: {},
    panes: {},
    agents: {},
    adapters: [],
    notificationWatermark: 0n,
  };
}

export function createSessionStore(): SessionStore {
  return createStore<SessionState & SessionActions>((set, get) => ({
    ...initialSessionState(),

    setConnection(connection) {
      set((state) => ({ connection: { ...state.connection, ...connection } }));
    },

    setServerIdentity(serverIdentity) {
      set({ serverIdentity });
    },

    applyTopologyAck(generation) {
      // Never backwards: an ack that raced a full snapshot must not undo it.
      if (generation > get().topologyGeneration) set({ topologyGeneration: generation });
    },

    applySnapshot(snapshot) {
      const sessions: Record<string, Session> = {};
      for (const session of [...snapshot.sessions].sort((a, b) => a.order - b.order)) {
        sessions[session.id] = {
          id: session.id,
          name: session.name,
          windowCount: session.windowCount,
          order: session.order,
          pinned: session.pinned,
        };
      }
      const windows: Record<string, Window> = {};
      for (const window of [...snapshot.windows].sort((a, b) => a.index - b.index)) {
        windows[window.id] = {
          id: window.id,
          sessionId: window.sessionId,
          index: window.index,
          name: window.name,
          active: window.active,
          pinned: window.pinned,
        };
      }
      const panes: Record<string, Pane> = {};
      for (const pane of [...snapshot.panes].sort((a, b) => a.index - b.index)) {
        panes[pane.id] = {
          id: pane.id,
          sessionId: pane.sessionId,
          windowId: pane.windowId,
          index: pane.index,
          active: pane.active,
          width: pane.width,
          height: pane.height,
          left: pane.left,
          top: pane.top,
          currentPath: pane.currentPath,
          currentCommand: pane.currentCommand,
        };
      }
      set({
        serverIdentity: snapshot.serverIdentity,
        topologyGeneration: snapshot.generation,
        sessions,
        windows,
        panes,
      });
      if (snapshot.agents?.authoritative) get().applyAgentSnapshot(snapshot.agents);
    },

    applyAgentSnapshot(snapshot) {
      if (!snapshot.authoritative) return [];
      const previous = get().agents;
      const agents: Record<string, Agent> = {};
      const transitions: AgentTransition[] = [];
      for (const record of snapshot.agents) {
        const next = agentFromProto(record);
        agents[next.id] = next;
        transitions.push({ prev: previous[next.id], next });
      }
      set({
        agents,
        adapters: snapshot.adapters.map(adapterFromProto),
        notificationWatermark: snapshot.notificationWatermark,
      });
      return transitions;
    },

    applyAgentEvent(event) {
      const agents = { ...get().agents };
      for (const retired of event.retiredAgentIds) delete agents[retired];
      let transition: AgentTransition | undefined;
      if (event.agent) {
        const next = agentFromProto(event.agent);
        const prev = get().agents[next.id];
        if (prev === undefined || next.stateGeneration >= prev.stateGeneration) {
          agents[next.id] = next;
          transition = { prev, next };
        }
      }
      set({ agents });
      return transition;
    },

    setFocusedPane(paneId) {
      set({ focusedPaneId: paneId });
    },

    clearHostState() {
      const { connection, focusedPaneId } = get();
      set({ ...initialSessionState(), connection, focusedPaneId });
    },
  }));
}

/** The app-wide instance. Tests create their own with `createSessionStore()`. */
export const sessionStore: SessionStore = createSessionStore();

/** Mirrors `lifecycle_label` in apps/desktop/src-tauri/src/connection/agent.rs. */
export function lifecycleFromProto(value: AgentLifecycleState): AgentLifecycle {
  switch (value) {
    case AgentLifecycleState.WORKING:
      return "working";
    case AgentLifecycleState.BLOCKED:
      return "blocked";
    case AgentLifecycleState.IDLE:
      return "idle";
    default:
      return "unknown";
  }
}

function attentionKindFromProto(value: string): AgentAttentionKind {
  return value === "blocked" || value === "completed" ? value : "";
}

export function agentFromProto(record: ProtoAgentRecord): Agent {
  const route = record.route;
  return {
    id: record.agentId,
    adapterId: record.adapterId,
    displayName: record.displayName,
    lifecycle: lifecycleFromProto(record.lifecycle),
    attentionKind: attentionKindFromProto(record.attentionKind),
    stateGeneration: record.stateGeneration,
    attentionGeneration: record.attentionGeneration,
    seenGeneration: record.seenGeneration,
    updatedAtMs: Number(record.updatedAtUnixMillis),
    // A helper from before the field decodes it as zero; its update time is
    // the only clock it can offer.
    lifecycleChangedAtMs: Number(record.lifecycleChangedAtUnixMillis) || Number(record.updatedAtUnixMillis),
    attentionSeenAtMs: Number(record.attentionSeenAtUnixMillis),
    present: record.present,
    route: {
      sessionId: route?.sessionId ?? "",
      sessionNameFallback: route?.sessionNameFallback ?? "",
      windowId: route?.windowId ?? "",
      windowNameFallback: route?.windowNameFallback ?? "",
      paneId: route?.paneId ?? "",
      paneIndexFallback: route?.paneIndexFallback ?? 0,
    },
  };
}

function adapterFromProto(descriptor: ProtoAgentAdapterDescriptor): AgentAdapterDescriptor {
  return { id: descriptor.id, displayName: descriptor.displayName, hookWiring: hookWiringFromProto(descriptor.hookWiring) };
}

function hookWiringFromProto(value: AgentHookWiring): AgentHookWiringState {
  switch (value) {
    case AgentHookWiring.WIRED:
      return "wired";
    case AgentHookWiring.PARTIAL:
      return "partial";
    case AgentHookWiring.NOT_WIRED:
      return "notWired";
    case AgentHookWiring.UNAVAILABLE:
      return "unavailable";
    case AgentHookWiring.ABSENT:
      return "absent";
    default:
      return "unspecified";
  }
}
