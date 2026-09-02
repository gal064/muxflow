import type { AgentGeneration } from "./generation";

export type AgentAdapterId = string & {};
export type AgentLifecycle = "working" | "blocked" | "idle" | "unknown";
export type AgentAttentionKind = "blocked" | "completed";
export type AgentDisplayState = AgentLifecycle | "done";
export type AgentPlacement = "window" | "split";

/**
 * What this host's configuration actually does with the adapter's lifecycle
 * events, observed by the daemon rather than claimed by the adapter.
 *
 * `unavailable` is not `notWired`: a configuration nobody could read might be
 * wired perfectly, and offering to write over it is how unrelated hooks get
 * lost. `unspecified` is a host too old to answer.
 */
/** The states the host can report, once. `mapHookWiring` validates against it. */
export const AGENT_HOOK_WIRINGS = ["wired", "partial", "notWired", "absent", "unavailable", "unspecified"] as const;
export type AgentHookWiring = typeof AGENT_HOOK_WIRINGS[number];

export interface AgentAdapterDescriptor {
  id: AgentAdapterId;
  displayName: string;
  supportsLaunch: boolean;
  supportsResume: boolean;
  supportsHooks: boolean;
  supportsProcessDetection: boolean;
  hookConfigPath: string;
  hookEvents: string[];
  placements: AgentPlacement[];
  hookWiring: AgentHookWiring;
  /** Why the wiring could not be read; empty in every other state. */
  hookWiringDetail: string;
  /**
   * Whether the host says an install would act on this adapter. The host is
   * what observed the wiring, so the host decides; the desktop used to keep a
   * second copy of the rule and the two had already diverged.
   */
  hookSetupRecommended: boolean;
}

export interface AgentRoute {
  hostProfileId: string;
  serverIdentity: string;
  sessionId: string;
  sessionName: string;
  windowId: string;
  windowName: string;
  /** Exact routable pane; empty when the hook was absent from topology. */
  paneId: string;
}

/** Adapter-independent latest state supplied by the host. */
export interface AgentRecord extends AgentRoute {
  id: string;
  adapterId: AgentAdapterId;
  nativeSessionId: string;
  displayName: string;
  lifecycle: AgentLifecycle;
  lifecycleGeneration: AgentGeneration;
  attentionGeneration: AgentGeneration;
  /** Persisted cause of the current attention generation. */
  attentionKind?: AgentAttentionKind;
  seenGeneration: AgentGeneration;
  updatedAt: number;
  /** Host-observed time when `lifecycle` last changed. */
  lifecycleChangedAt: number;
  /** Host-observed time when the current attention generation was first seen. */
  attentionSeenAt: number;
  detectedManually: boolean;
  present: boolean;
}

export interface AgentSnapshot {
  hostProfileId: string;
  serverIdentity: string;
  connectionEpoch: number;
  revision: AgentGeneration;
  eventSequence: AgentGeneration;
  /** Host barrier: every live mutation at or below this generation is included. */
  acceptedGeneration: AgentGeneration;
  notificationWatermark: AgentGeneration;
  authoritative: true;
  agents: AgentRecord[];
  adapters: AgentAdapterDescriptor[];
}

/** The tmux topology for which an accepted agent snapshot can prove absence. */
export interface AgentTopologyAuthority {
  hostProfileId: string;
  serverIdentity: string;
  connectionEpoch: number;
  topologyGeneration: number;
  coveredWindowIds: ReadonlySet<string>;
}

export type AgentWireEvent =
  | { kind: "snapshot"; snapshot: AgentSnapshot; replayed?: boolean }
  | { kind: "upsert"; hostProfileId: string; serverIdentity: string; connectionEpoch: number; sequence: AgentGeneration; record: AgentRecord; retiredAgentIds?: readonly string[]; replayed?: boolean }
  /** Retirements with no surviving record: the host saw the agent's process go. */
  | { kind: "retired"; hostProfileId: string; serverIdentity: string; connectionEpoch: number; sequence: AgentGeneration; retiredAgentIds: readonly string[]; replayed?: boolean }
  | { kind: "removed"; hostProfileId: string; serverIdentity: string; connectionEpoch: number; sequence: AgentGeneration; agentId: string; updatedAt: number; replayed?: boolean };

/** One host's agent records; `hostProfileId` is unset until its first snapshot. */
export interface HostAgentState {
  hostProfileId?: string;
  serverIdentity?: string;
  connectionEpoch?: number;
  snapshotRevision: AgentGeneration;
  eventSequence: AgentGeneration;
  authoritative: boolean;
  byId: Readonly<Record<string, AgentRecord>>;
  adapters: readonly AgentAdapterDescriptor[];
}

export interface AgentStoreState {
  byHost: Readonly<Record<string, HostAgentState>>;
}

export interface AgentAttentionRollup {
  state: "none" | AgentDisplayState;
  /**
   * Whose adapter the `state` came from — the loudest agent in the group, and
   * the same one the state is reporting on. A rollup that says "blocked" and an
   * icon taken from some other agent in the window would be two facts about two
   * agents drawn as one. Absent only for an empty rollup.
   */
  adapterId?: AgentAdapterId;
  blocked: number;
  working: number;
  done: number;
  unknown: number;
  idle: number;
  total: number;
}

export interface AgentRollups {
  byAgent: ReadonlyMap<string, AgentAttentionRollup>;
  byPane: ReadonlyMap<string, AgentAttentionRollup>;
  byWindow: ReadonlyMap<string, AgentAttentionRollup>;
  byWorkspace: ReadonlyMap<string, AgentAttentionRollup>;
}

export interface AgentFocus {
  hostProfileId: string;
  serverIdentity?: string;
  sessionId?: string;
  windowId?: string;
  paneId?: string;
  appFocused: boolean;
  terminalVisible: boolean;
  automaticSeen: boolean;
}

export interface AgentLaunchRequest {
  adapterId: AgentAdapterId;
  placement: AgentPlacement;
  sessionId: string;
  windowId: string;
  paneId: string;
  activeRoot: string;
  rootToken: string;
}

export interface AgentHookChange {
  path: string;
  summary: string;
  owner: string;
  command: string;
  events: string[];
  beforeHash: string;
  afterHash: string;
  createsConfig: boolean;
  removesConfig: boolean;
  beforePreview: string;
  afterPreview: string;
  diffPreview: string;
  previewTruncated: boolean;
}

export interface AgentHookReview {
  adapterId: AgentAdapterId;
  adapterDisplayName?: string;
  action: "install" | "uninstall";
  revision: string;
  alreadyInstalled: boolean;
  changes: AgentHookChange[];
  managedLabel: string;
  trustGuidance?: string;
  backupPath?: string;
}

/**
 * What the host did with the recommended tmux window naming.
 *
 * Two of the three are successes. `alreadyCurrent` is this app's own hook,
 * already covering every adapter it knows about. `userConfigured` is the
 * user's own arrangement, kept — it may carry exemptions this app knows
 * nothing about.
 */
export const AGENT_HOST_NAMING_OUTCOMES = ["applied", "alreadyCurrent", "userConfigured", "removed", "nothingToRemove"] as const;
/** `unavailable` is this side's answer to a value the host did not give. */
export type AgentHostNamingOutcome = typeof AGENT_HOST_NAMING_OUTCOMES[number] | "unavailable";

export interface AgentRequestScope {
  clientId: string;
  hostProfileId: string;
  serverIdentity: string;
  topologyGeneration: number;
  connectionEpoch: number;
}

/** A request scope plus the tmux windows its snapshot can prove absence for. */
export interface AgentRuntimeScope {
  scope: AgentRequestScope;
  topologyWindowIds: readonly string[];
}

export interface AgentHostProjection {
  agents: readonly AgentRecord[];
  adapters: readonly AgentAdapterDescriptor[];
  rollups: AgentRollups;
}

/**
 * The host a consent answer belongs to, stable for as long as that host stays
 * connected.
 *
 * M13-E004: nothing tied the three separate reads of "the current host" that a
 * hook install performs — the decision the user answered for, the diff the host
 * computed, and the connection the write went down. Each was resolved when it
 * was reached, so a host switch anywhere in between let a host that had never
 * been asked receive the write *and* the recorded "accepted". The persisted
 * decision stays keyed on `hostProfileId`, which is what survives a
 * reconnection; this is the narrower key that has to hold still for the
 * duration of one install.
 *
 * `topologyGeneration` is deliberately excluded: it moves on every pane change
 * and would make an install racing an unrelated split fail for nothing.
 */
export function agentHostIdentity(scope?: AgentRequestScope): string | undefined {
  return scope && `${scope.hostProfileId}\0${scope.clientId}\0${scope.connectionEpoch}`;
}

export interface AgentSoundPreferences {
  enabled: boolean;
  blocked: "subtle" | "none";
  completed: "subtle" | "none";
  volume: number;
}

export const defaultAgentSoundPreferences: AgentSoundPreferences = {
  enabled: true,
  blocked: "subtle",
  completed: "subtle",
  volume: 0.45,
};

export interface AgentNotificationRoute extends AgentRoute {
  agentId: string;
  attentionGeneration: AgentGeneration;
}

export interface AgentNativeNotification {
  event: "blocked" | "completed";
  title: string;
  body: string;
  route: AgentNotificationRoute;
  /** False for an unmapped record that must remain in-app-only attention. */
  requestAction: boolean;
  /**
   * Whether the OS should show this while the app is frontmost. Only this side
   * knows which pane the user is looking at, so only this side can answer it.
   */
  presentInForeground: boolean;
}

export interface AgentNotificationInstrumentation {
  outcome: "emitted" | "suppressed-focused" | "suppressed-replay" | "suppressed-duplicate" | "failed";
  agentId: string;
  event: "blocked" | "completed";
  generation: AgentGeneration;
  actionable?: boolean;
  error?: string;
}
