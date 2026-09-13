import { invoke } from "@tauri-apps/api/core";
import { recordIncident } from "../../diagnostics/incidents";
import { agentGeneration, zeroGeneration, type AgentGeneration } from "./generation";
import { canonicalAdapterId } from "./adapterDefinitions";
import { AGENT_HOOK_WIRINGS, AGENT_HOST_NAMING_OUTCOMES } from "./types";
import type {
  AgentAdapterDescriptor,
  AgentAdapterId,
  AgentHookReview,
  AgentHookWiring,
  AgentHostNamingOutcome,
  AgentLaunchRequest,
  AgentLifecycle,
  AgentRecord,
  AgentRequestScope,
  AgentSnapshot,
  AgentWireEvent,
} from "./types";

interface WireRoute {
  hostProfileId: string;
  serverIdentity: string;
  sessionId: string;
  sessionNameFallback: string;
  windowId: string;
  windowNameFallback: string;
  paneId: string;
  paneIndexFallback?: number;
  agentId: string;
  attentionGeneration: string | number;
}

interface WireRecord {
  agentId: string;
  adapterId?: string;
  nativeSessionId: string;
  displayName: string;
  route: WireRoute;
  lifecycle: string;
  stateGeneration: string | number;
  attentionGeneration: string | number;
  attentionKind?: string;
  seenGeneration: string | number;
  updatedAtUnixMillis: string | number;
  lifecycleChangedAtUnixMillis: string | number;
  attentionSeenAtUnixMillis: string | number;
  detectedManually: boolean;
  present: boolean;
}

export interface WireAgentSnapshot {
  generation: string | number;
  acceptedGeneration: string | number;
  agents: WireRecord[];
  authoritative: boolean;
  notificationWatermark?: string | number;
  connectionEpoch: string | number;
  adapters?: Array<{
    id: string; displayName: string; supportsLaunch?: boolean; supportsResume?: boolean;
    supportsHooks?: boolean; supportsProcessDetection?: boolean;
    hookConfigPath?: string; hookEvents?: string[];
    hookWiring?: string; hookWiringDetail?: string; hookSetupRecommended?: boolean;
  }>;
}

interface WireHookPlan {
  adapterId?: string;
  action: string;
  configPath: string;
  backupPath?: string;
  managedVersion: string;
  summary: string;
  confirmationToken: string;
  alreadyCurrent: boolean;
  ownershipMarker?: string;
  proposedCommand?: string;
  proposedEvents?: string[];
  trustGuidance?: string;
  beforeHash?: string;
  afterHash?: string;
  createsConfig?: boolean;
  removesConfig?: boolean;
  beforePreview?: string;
  afterPreview?: string;
  diffPreview?: string;
  previewTruncated?: boolean;
}

interface WireResponse {
  snapshot?: WireAgentSnapshot;
  agent?: WireRecord;
  hookPlan?: WireHookPlan;
  sessionId?: string;
  windowId?: string;
  paneId?: string;
  acceptedGeneration?: string | number;
  hostNaming?: string;
}

export interface WireAgentEvent {
  agent?: WireRecord;
  generation: string | number;
  notify?: boolean;
  reason?: string;
  retiredAgentIds?: string[];
  connectionEpoch: string | number;
}

export interface AgentClient {
  snapshot(scope: AgentRequestScope): Promise<AgentSnapshot>;
  launch(scope: AgentRequestScope, request: AgentLaunchRequest): Promise<void>;
  resume(scope: AgentRequestScope, agentId: string, nativeSessionId: string, request: AgentLaunchRequest): Promise<void>;
  rename(scope: AgentRequestScope, agentId: string, displayName: string): Promise<void>;
  markSeen(scope: AgentRequestScope, agentId: string, attentionGeneration: AgentGeneration): Promise<void | AgentRecord>;
  reviewHooks(scope: AgentRequestScope, adapter: AgentAdapterId, action?: "install" | "uninstall"): Promise<AgentHookReview>;
  applyHooks(scope: AgentRequestScope, review: AgentHookReview): Promise<void>;
  applyHostNaming(scope: AgentRequestScope, action?: "install" | "uninstall"): Promise<AgentHostNamingOutcome>;
  publishWireEvent(scope: AgentRequestScope, event: WireAgentEvent): void;
  publishWireSnapshot(scope: AgentRequestScope, snapshot: WireAgentSnapshot): void;
  subscribe(listener: (event: AgentWireEvent) => void): () => void;
}

/** Thin Tauri transport; lifecycle meaning remains in the adapter-neutral store. */
export class TauriAgentClient implements AgentClient {
  readonly #listeners = new Set<(event: AgentWireEvent) => void>();

  async snapshot(scope: AgentRequestScope): Promise<AgentSnapshot> {
    const response = await this.#request(scope, { operation: "snapshot" });
    if (!response.snapshot) throw new Error("Host omitted the authoritative agent snapshot.");
    return mapSnapshot(scope, response.snapshot);
  }

  async launch(scope: AgentRequestScope, request: AgentLaunchRequest): Promise<void> {
    await this.#request(scope, {
      operation: request.placement === "window" ? "launchWindow" : "launchSplit",
      adapterId: request.adapterId,
      sessionId: request.sessionId,
      windowId: request.windowId,
      paneId: request.paneId,
      activeRoot: request.activeRoot,
      rootToken: request.rootToken,
    });
  }

  async resume(scope: AgentRequestScope, agentId: string, nativeSessionId: string, request: AgentLaunchRequest): Promise<void> {
    await this.#request(scope, {
      operation: "resume", agentId, nativeSessionId, adapterId: request.adapterId,
      placement: request.placement, sessionId: request.sessionId, windowId: request.windowId,
      paneId: request.placement === "split" ? request.paneId : "",
      activeRoot: request.activeRoot, rootToken: request.rootToken,
    });
  }

  async rename(scope: AgentRequestScope, agentId: string, displayName: string): Promise<void> {
    const normalized = displayName.trim();
    if (!normalized) throw new Error("Agent name cannot be empty.");
    const response = await this.#request(scope, { operation: "rename", agentId, displayName: normalized });
    if (!response.agent || response.acceptedGeneration === undefined) throw new Error("Host omitted the accepted rename mutation.");
    this.#publish({
      kind: "upsert", hostProfileId: scope.hostProfileId, serverIdentity: scope.serverIdentity, connectionEpoch: scope.connectionEpoch,
      sequence: agentGeneration(response.acceptedGeneration, "agent response accepted generation"),
      record: mapRecord(scope, response.agent), replayed: false,
    });
  }

  async markSeen(scope: AgentRequestScope, agentId: string, attentionGeneration: AgentGeneration): Promise<void | AgentRecord> {
    if (attentionGeneration === zeroGeneration) return;
    const response = await this.#request(scope, { operation: "markSeen", agentId, attentionGeneration });
    if (!response.agent || response.acceptedGeneration === undefined) throw new Error("Host omitted the accepted seen mutation.");
    const record = mapRecord(scope, response.agent);
    this.#publish({
      kind: "upsert", hostProfileId: scope.hostProfileId, serverIdentity: scope.serverIdentity, connectionEpoch: scope.connectionEpoch,
      sequence: agentGeneration(response.acceptedGeneration, "agent response accepted generation"),
      record, replayed: false,
    });
    return record;
  }

  async reviewHooks(scope: AgentRequestScope, adapter: AgentAdapterId, action: "install" | "uninstall" = "install"): Promise<AgentHookReview> {
    const response = await this.#request(scope, { operation: "hookReview", adapterId: adapter, hookManagementTarget: action });
    const plan = response.hookPlan;
    if (!plan?.confirmationToken || !plan.configPath) throw new Error("Host omitted the reviewable hook plan.");
    return {
      adapterId: canonicalAdapterId(plan.adapterId),
      action,
      revision: plan.confirmationToken,
      alreadyInstalled: Boolean(plan.alreadyCurrent),
      changes: [{
        path: plan.configPath,
        summary: plan.summary,
        owner: plan.ownershipMarker ?? "",
        command: plan.proposedCommand ?? "",
        events: plan.proposedEvents ?? [],
        beforeHash: plan.beforeHash ?? "",
        afterHash: plan.afterHash ?? "",
        createsConfig: Boolean(plan.createsConfig),
        removesConfig: Boolean(plan.removesConfig),
        beforePreview: plan.beforePreview ?? "",
        afterPreview: plan.afterPreview ?? "",
        diffPreview: plan.diffPreview ?? "",
        previewTruncated: Boolean(plan.previewTruncated),
      }],
      managedLabel: plan.managedVersion,
      ...(plan.trustGuidance ? { trustGuidance: plan.trustGuidance } : {}),
      ...(plan.backupPath ? { backupPath: plan.backupPath } : {}),
    };
  }

  async applyHostNaming(scope: AgentRequestScope, action: "install" | "uninstall" = "install"): Promise<AgentHostNamingOutcome> {
    const response = await this.#request(scope, { operation: action === "install" ? "hostNaming" : "hostNamingRemove" });
    const known = AGENT_HOST_NAMING_OUTCOMES.find((outcome) => outcome === response.hostNaming);
    return known ?? "unavailable";
  }

  async applyHooks(scope: AgentRequestScope, review: AgentHookReview): Promise<void> {
    await this.#request(scope, {
      operation: review.action === "install" ? "hookInstall" : "hookUninstall",
      adapterId: review.adapterId,
      confirmed: true,
      confirmationToken: review.revision,
    });
  }

  publishWireEvent(scope: AgentRequestScope, event: WireAgentEvent): void {
    if (!sameWireConnectionEpoch(scope, event.connectionEpoch)) return;
    const retiredAgentIds = event.retiredAgentIds ?? [];
    // An event may carry retirements and no record: that is how the host says
    // an agent's process is gone. Returning early on a missing record dropped
    // those on the floor, and the row only ever disappeared because a full
    // snapshot happened to follow.
    if (!event.agent) {
      if (retiredAgentIds.length === 0) return;
      this.#publish({
        kind: "retired",
        hostProfileId: scope.hostProfileId,
        serverIdentity: scope.serverIdentity,
        connectionEpoch: scope.connectionEpoch,
        sequence: agentGeneration(event.generation, "agent event generation"),
        retiredAgentIds,
        replayed: event.notify === false,
      });
      return;
    }
    const record = mapRecord(scope, event.agent);
    // Live transitions only (replays re-state old facts): this line is how a
    // "stuck working/blocked" tab is diagnosed after the fact — the last state
    // the host asserted, its reason, and whether anything followed it.
    if (event.notify !== false) {
      recordIncident("agent.state", {
        id: record.id,
        lifecycle: record.lifecycle,
        reason: event.reason,
        attentionKind: record.attentionKind,
        paneId: record.paneId,
        windowId: record.windowId,
      });
    }
    this.#publish({
      kind: "upsert",
      hostProfileId: scope.hostProfileId,
      serverIdentity: scope.serverIdentity,
      connectionEpoch: scope.connectionEpoch,
      sequence: agentGeneration(event.generation, "agent event generation"),
      record,
      retiredAgentIds,
      replayed: event.notify === false,
    });
  }

  publishWireSnapshot(scope: AgentRequestScope, snapshot: WireAgentSnapshot): void {
    if (!sameWireConnectionEpoch(scope, snapshot.connectionEpoch)) return;
    this.#publish({ kind: "snapshot", snapshot: mapSnapshot(scope, snapshot), replayed: true });
  }

  subscribe(listener: (event: AgentWireEvent) => void): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  #publish(event: AgentWireEvent): void {
    for (const listener of this.#listeners) listener(event);
  }

  #request(scope: AgentRequestScope, command: Record<string, unknown>): Promise<WireResponse> {
    return invoke("agent_request", {
      clientId: scope.clientId,
      command: {
        ...command,
        expectedServerIdentity: scope.serverIdentity,
        expectedTopologyGeneration: String(scope.topologyGeneration),
        connectionEpoch: String(scope.connectionEpoch),
        hostProfileId: scope.hostProfileId,
      },
    });
  }
}

function mapSnapshot(scope: AgentRequestScope, value: WireAgentSnapshot): AgentSnapshot {
  if (!value.authoritative || !Array.isArray(value.agents)) throw new Error("Host returned a non-authoritative agent snapshot.");
  if (!sameWireConnectionEpoch(scope, value.connectionEpoch)) throw new Error("Host returned an agent snapshot for a replaced connection epoch.");
  const revision = agentGeneration(value.generation, "agent snapshot generation");
  const acceptedGeneration = agentGeneration(value.acceptedGeneration, "agent snapshot accepted generation");
  return {
    hostProfileId: scope.hostProfileId,
    serverIdentity: scope.serverIdentity,
    connectionEpoch: scope.connectionEpoch,
    revision,
    eventSequence: acceptedGeneration,
    acceptedGeneration,
    notificationWatermark: agentGeneration(value.notificationWatermark ?? value.generation, "agent notification watermark"),
    authoritative: true,
    agents: value.agents.map((agent) => mapRecord(scope, agent)),
    adapters: (value.adapters ?? []).map(mapAdapterDescriptor),
  };
}

function mapRecord(scope: AgentRequestScope, value: WireRecord): AgentRecord {
  if (!value.route || !value.agentId || value.route.agentId !== value.agentId
    || value.route.hostProfileId !== scope.hostProfileId
    || value.route.serverIdentity !== scope.serverIdentity) throw new Error("Host returned a malformed agent route.");
  const attentionGeneration = agentGeneration(value.attentionGeneration, "agent attention generation");
  if (agentGeneration(value.route.attentionGeneration, "agent route generation") !== attentionGeneration) {
    throw new Error("Agent route generation conflicts with its record.");
  }
  const updatedAt = safeNumber(value.updatedAtUnixMillis, "agent update time");
  const lifecycleChangedAt = safeNumber(value.lifecycleChangedAtUnixMillis, "agent lifecycle change time");
  const attentionSeenAt = safeNumber(value.attentionSeenAtUnixMillis, "agent attention seen time");
  return {
    id: value.agentId,
    adapterId: canonicalAdapterId(value.adapterId),
    nativeSessionId: value.nativeSessionId,
    displayName: value.displayName || `${value.adapterId || "Agent"} agent`,
    hostProfileId: scope.hostProfileId,
    serverIdentity: scope.serverIdentity,
    sessionId: value.route.sessionId,
    sessionName: value.route.sessionNameFallback,
    windowId: value.route.windowId,
    windowName: value.route.windowNameFallback,
    paneId: value.route.paneId,
    lifecycle: mapLifecycle(value.lifecycle),
    lifecycleGeneration: agentGeneration(value.stateGeneration, "agent state generation"),
    attentionGeneration,
    ...mapAttentionKind(value.attentionKind),
    seenGeneration: agentGeneration(value.seenGeneration, "agent seen generation"),
    updatedAt,
    lifecycleChangedAt,
    attentionSeenAt,
    detectedManually: Boolean(value.detectedManually),
    present: Boolean(value.present),
  };
}

function mapAdapterDescriptor(value: NonNullable<WireAgentSnapshot["adapters"]>[number]): AgentAdapterDescriptor {
  const id = canonicalAdapterId(value.id);
  return {
    id, displayName: value.displayName || id,
    supportsLaunch: Boolean(value.supportsLaunch), supportsResume: Boolean(value.supportsResume),
    supportsHooks: Boolean(value.supportsHooks),
    supportsProcessDetection: Boolean(value.supportsProcessDetection),
    hookConfigPath: value.hookConfigPath ?? "", hookEvents: value.hookEvents ?? [],
    placements: value.supportsLaunch ? ["window", "split"] : [],
    hookWiring: mapHookWiring(value.hookWiring),
    hookWiringDetail: value.hookWiringDetail ?? "",
    hookSetupRecommended: Boolean(value.hookSetupRecommended),
  };
}

/**
 * An unrecognised value becomes `unspecified`, never a definite answer. A host
 * that speaks a wiring state this build does not know has told us nothing, and
 * guessing "not wired" would put an install prompt in front of the user for a
 * configuration that may already be correct.
 */
function mapHookWiring(value: string | undefined): AgentHookWiring {
  const known = AGENT_HOOK_WIRINGS.find((state) => state === value);
  return known ?? "unspecified";
}

/**
 * A value this build does not know degrades to `unknown`, which is already the
 * product's word for "nothing here can say what this agent is doing".
 *
 * These used to throw. Nothing catches per record, so `mapSnapshot` aborted on
 * the first unrecognised value and the user's entire agent list went blank —
 * a far worse outcome than one row reading `unknown`, and one that a host
 * merely newer than the desktop could cause.
 */
function mapLifecycle(value: string): AgentLifecycle {
  if (value === "working" || value === "blocked" || value === "idle") return value;
  return "unknown";
}

/** An unrecognised kind is no attention rather than a thrown snapshot. */
function mapAttentionKind(value: string | undefined): Pick<AgentRecord, "attentionKind"> {
  if (value === "blocked" || value === "completed") return { attentionKind: value };
  return {};
}

function safeNumber(value: string | number, label: string): number {
  const number = typeof value === "number" ? value : Number(value);
  if (!Number.isSafeInteger(number) || number < 0) throw new Error(`${label} is outside the JavaScript safe range.`);
  return number;
}

function sameWireConnectionEpoch(scope: AgentRequestScope, value: string | number): boolean {
  return String(value) === String(scope.connectionEpoch);
}
