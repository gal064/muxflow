import type { TerminalEvent } from "./api";
import type { OperationRecorder } from "../../perf/operations";

type EpochEvent = Extract<TerminalEvent, { kind: "generationEpoch" }>;
type EpochListener = (event: EpochEvent) => void;
type PaneEvent = Extract<TerminalEvent, { kind: "seed" | "output" | "paneResource" | "seedDiagnostic" }>;
type PaneListener = (event: PaneEvent) => void;

const DEFAULT_MAX_PANE_BYTES = 8 * 1024 * 1024;
const DEFAULT_MAX_TOTAL_BYTES = 16 * 1024 * 1024;
const DEFAULT_MAX_BUFFERED_PANES = 32;
const DEFAULT_MAX_PANE_EVENTS = 65_536;
const diagnosticEncoder = new TextEncoder();

interface PaneBacklogEntry {
  event: PaneEvent;
  byteLength: number;
}

interface PaneBacklog {
  entries: PaneBacklogEntry[];
  byteLength: number;
}

interface PaneStreamState {
  backlog?: PaneBacklog;
  lastGeneration: number;
  lastPaneResource?: PaneResourceIdentity;
  renderedGeneration: number;
  awaitingSeed: boolean;
  conflictReseedRequested: boolean;
}

function createPaneStreamState(): PaneStreamState {
  return {
    lastGeneration: -1,
    renderedGeneration: 0,
    awaitingSeed: false,
    conflictReseedRequested: false,
  };
}

interface PaneResourceIdentity {
  state: Extract<PaneEvent, { kind: "paneResource" }>["state"];
  requiresSeed: boolean;
  generation: number;
  snapshotGeneration: number;
  tailThroughGeneration: number;
  recoveryReason: string;
  serializedSnapshot: Uint8Array;
  rawTail: Uint8Array;
}

const MAX_EXACT_RESOURCE_IDENTITY_BYTES = 256 * 1024;

export interface TerminalEventHubLimits {
  maxPaneBytes?: number;
  maxTotalBytes?: number;
  maxBufferedPanes?: number;
  maxTrackedPanes?: number;
  maxPaneEvents?: number;
}

export type TerminalEventAdmission =
  | { kind: "accepted" | "local" }
  | { kind: "stale" }
  | { kind: "gap"; expected: number; received: number };

/**
 * Routes pane events without retaining an unbounded all-host history. A Map's
 * insertion order is the hidden-pane LRU; subscribing consumes that entry.
 */
export class TerminalEventHub {
  readonly #epochListeners = new Set<EpochListener>();
  readonly #paneListeners = new Map<string, PaneListener>();
  readonly #paneStates = new Map<string, PaneStreamState>();
  readonly #evictedSeedDebt = new Map<string, true>();
  readonly #maxPaneBytes: number;
  readonly #maxTotalBytes: number;
  readonly #maxBufferedPanes: number;
  readonly #maxTrackedPanes: number;
  readonly #maxPaneEvents: number;
  #backlogBytes = 0;
  #backlogCount = 0;
  #generationEpoch?: number;
  #lastSequence = 0;
  #sequenceFrozen = false;
  #unknownPanesRequireSeed = false;

  constructor(
    readonly onSeedRequired?: (paneId: string, reason: string) => void,
    limits: TerminalEventHubLimits = {},
    readonly measurements?: OperationRecorder,
  ) {
    this.#maxPaneBytes = limits.maxPaneBytes ?? DEFAULT_MAX_PANE_BYTES;
    this.#maxTotalBytes = limits.maxTotalBytes ?? DEFAULT_MAX_TOTAL_BYTES;
    this.#maxBufferedPanes = limits.maxBufferedPanes ?? DEFAULT_MAX_BUFFERED_PANES;
    // A pane cannot be buffered safely without one metadata slot: otherwise
    // the event that creates its backlog evicts its own generation/debt state.
    this.#maxTrackedPanes = Math.max(1, limits.maxTrackedPanes ?? Math.max(64, this.#maxBufferedPanes * 4));
    this.#maxPaneEvents = limits.maxPaneEvents ?? DEFAULT_MAX_PANE_EVENTS;
  }

  publish(event: TerminalEvent, beforeDelivery?: () => void): TerminalEventAdmission {
    this.measurements?.add("terminal.hub.events");
    if (event.kind === "seed" || event.kind === "output") {
      this.measurements?.add("terminal.hub.payloadBytes", event.data.byteLength);
    }
    const epochChanged = event.kind === "generationEpoch" && event.epoch !== this.#generationEpoch;
    const admission = this.#admitSequence(event);
    if (admission.kind === "stale" || admission.kind === "gap") return admission;
    if (epochChanged && event.kind === "generationEpoch") {
      this.#generationEpoch = event.epoch;
      this.#clearPaneState();
    }
    if (event.kind !== "generationEpoch" || epochChanged) beforeDelivery?.();
    if (epochChanged && event.kind === "generationEpoch") {
      for (const listener of this.#epochListeners) {
        this.measurements?.add("terminal.hub.epochDeliveries");
        listener(event);
      }
    }
    if (event.kind === "generationEpoch") return admission;
    if (event.kind !== "seed" && event.kind !== "output" && event.kind !== "paneResource" && event.kind !== "seedDiagnostic") return admission;
    const wasTracked = this.#paneStates.has(event.paneId);
    const hadEvictedSeedDebt = this.#evictedSeedDebt.delete(event.paneId);
    const requiresConservativeSeed = !wasTracked && this.#unknownPanesRequireSeed;
    const pane = this.#touchPane(event.paneId);
    if (hadEvictedSeedDebt || requiresConservativeSeed) {
      const alreadyAwaiting = pane.awaitingSeed;
      pane.awaitingSeed = true;
      this.#deleteBacklog(pane);
      // Neither incremental output nor an empty handoff can repair content
      // discarded with an evicted hidden backlog. Do not let either advance
      // the generation watermark ahead of the fresh seed we already owe.
      const cannotRepairDebt = event.kind === "output"
        || (event.kind === "paneResource" && !event.requiresSeed
          && event.serializedSnapshot.byteLength + event.rawTail.byteLength === 0);
      if (cannotRepairDebt) {
        if (requiresConservativeSeed && !hadEvictedSeedDebt && !alreadyAwaiting) {
          this.onSeedRequired?.(event.paneId, "frontend pane recovery debt outlived the metadata LRU");
        }
        return admission;
      }
    }
    if (event.kind !== "seedDiagnostic") {
      const lastGeneration = pane.lastGeneration;
      if (event.kind === "paneResource") {
        const resourceIdentity = copyPaneResourceIdentity(event);
        if (event.generation <= lastGeneration) {
          const previous = pane.lastPaneResource;
          if (event.generation < lastGeneration || !previous || !resourceIdentity || !samePaneResource(previous, resourceIdentity)) {
            this.#requestConflictReseed(event.paneId, pane);
          }
          return admission;
        }
        pane.lastGeneration = event.generation;
        pane.lastPaneResource = resourceIdentity;
      } else if (event.generation <= lastGeneration) {
        // A seed is authoritative content, not an increment: dropping one
        // because its generation looks stale leaves the pane waiting for a
        // screen that has already been sent and will not be sent again
        // (P12-U003.3). Ask for one that this hub can accept instead.
        if (event.kind === "seed") this.#requestConflictReseed(event.paneId, pane);
        return admission;
      } else {
        pane.lastGeneration = event.generation;
        pane.lastPaneResource = undefined;
      }
      if (event.kind === "paneResource" && event.requiresSeed) {
        pane.awaitingSeed = true;
        this.#deleteBacklog(pane);
      } else if (event.kind === "seed") {
        pane.awaitingSeed = false;
        pane.conflictReseedRequested = false;
      } else if (event.kind === "paneResource") {
        // Recovery material is an authoritative replacement for a locally
        // evicted backlog. An empty reveal does not cancel a pending seed.
        if (event.serializedSnapshot.byteLength + event.rawTail.byteLength > 0) {
          pane.awaitingSeed = false;
          pane.conflictReseedRequested = false;
        } else if (pane.awaitingSeed) {
          return admission;
        }
      } else if (pane.awaitingSeed) {
        return admission;
      }
    }
    const listener = this.#paneListeners.get(event.paneId);
    if (listener) {
      this.measurements?.add("terminal.hub.fanoutDeliveries");
      listener(event);
      return admission;
    }
    this.#buffer(pane, event);
    return admission;
  }

  /**
   * Subscribes to admitted connection-epoch frames without joining the global
   * event fanout. Mounted panes use this lane so ordinary output is routed only
   * to the pane that owns it.
   */
  subscribeEpoch(listener: EpochListener): () => void {
    this.#epochListeners.add(listener);
    return () => this.#epochListeners.delete(listener);
  }

  subscribePane(paneId: string, listener: PaneListener): () => void {
    // Pane payload allocations are transferred to their renderer. A second
    // consumer would turn that move into mutable aliasing, so fail at the
    // ownership boundary instead of silently multicasting branded bytes.
    if (this.#paneListeners.has(paneId)) {
      throw new Error(`terminal pane ${paneId} already has an active consumer`);
    }
    this.#paneListeners.set(paneId, listener);
    const pane = this.#touchPane(paneId);
    const backlog = pane?.backlog;
    if (backlog) {
      this.#deleteBacklog(pane);
      for (const entry of backlog.entries) {
        this.measurements?.add("terminal.hub.fanoutDeliveries");
        listener(entry.event);
      }
      this.measurements?.add("terminal.hub.backlogDequeues", backlog.entries.length);
    }
    return () => {
      if (this.#paneListeners.get(paneId) === listener) this.#paneListeners.delete(paneId);
    };
  }

  clearPane(paneId: string): void {
    const pane = this.#paneStates.get(paneId);
    if (pane) this.#deleteBacklog(pane);
    this.#paneStates.delete(paneId);
    this.#evictedSeedDebt.delete(paneId);
  }

  clear(): void {
    this.#generationEpoch = undefined;
    this.#lastSequence = 0;
    this.#sequenceFrozen = false;
    this.#clearPaneState();
  }

  clearTerminalState(): void {
    this.#clearPaneState();
  }

  markRendered(paneId: string, generation: number, terminalEpoch = this.#generationEpoch): void {
    if (terminalEpoch === undefined || terminalEpoch !== this.#generationEpoch) return;
    if (!Number.isSafeInteger(generation) || generation < 0) return;
    const pane = this.#touchPane(paneId);
    if (generation > pane.renderedGeneration) pane.renderedGeneration = generation;
  }

  visibilityCheckpoint(paneId: string): { terminalEpoch: number; outputGeneration: number } | undefined {
    if (this.#generationEpoch === undefined) return undefined;
    return {
      terminalEpoch: this.#generationEpoch,
      outputGeneration: this.#paneStates.get(paneId)?.renderedGeneration ?? 0,
    };
  }

  get generationEpoch(): number | undefined {
    return this.#generationEpoch;
  }

  get lastSequence(): number {
    return this.#lastSequence;
  }

  get retainedPaneCount(): number {
    return this.#backlogCount;
  }

  get retainedByteLength(): number {
    return this.#backlogBytes;
  }

  get trackedPaneCount(): number {
    return this.#paneStates.size;
  }

  #clearPaneState(): void {
    this.#paneStates.clear();
    this.#backlogBytes = 0;
    this.#backlogCount = 0;
    this.#evictedSeedDebt.clear();
    this.#unknownPanesRequireSeed = false;
  }

  #buffer(pane: PaneStreamState, event: PaneEvent): void {
    this.measurements?.add("terminal.hub.backlogEnqueues");
    // Detach before mutating so aggregate accounting always describes the
    // bytes actually retained in the map, including overflow/reseed paths.
    const current = this.#takeBacklog(pane) ?? { entries: [], byteLength: 0 };
    if (event.kind === "seed") {
      this.#replaceBacklog(current, event, event.data.byteLength);
    } else if (event.kind === "paneResource") {
      const resourceBytes = event.serializedSnapshot.byteLength + event.rawTail.byteLength;
      const reasonBytes = diagnosticEncoder.encode(event.recoveryReason).byteLength;
      if (resourceBytes > 0) {
        this.#replaceBacklog(current, event, resourceBytes + reasonBytes);
      } else {
        this.#removeBacklogKind(current, "paneResource");
        this.#appendBacklog(current, event, reasonBytes);
      }
    } else if (event.kind === "seedDiagnostic") {
      this.#removeBacklogKind(current, "seedDiagnostic");
      this.#appendBacklog(current, event, diagnosticEncoder.encode(event.message).byteLength);
    } else {
      this.#appendBacklog(current, event, event.data.byteLength);
    }

    if (current.byteLength > this.#maxPaneBytes || current.entries.length > this.#maxPaneEvents) {
      const limit = this.#maxPaneBytes === DEFAULT_MAX_PANE_BYTES ? "8 MiB" : `${this.#maxPaneBytes} bytes`;
      this.#requireSeed(
        event.paneId,
        current.byteLength > this.#maxPaneBytes
          ? `frontend hidden recovery buffer exceeded ${limit}`
          : `frontend hidden recovery record capacity exceeded ${this.#maxPaneEvents} events`,
      );
      return;
    }
    this.#setBacklog(pane, current);
    this.#enforceBacklogLimits();
  }

  #appendBacklog(backlog: PaneBacklog, event: PaneEvent, byteLength: number): void {
    backlog.entries.push({ event, byteLength });
    backlog.byteLength += byteLength;
  }

  #replaceBacklog(backlog: PaneBacklog, event: PaneEvent, byteLength: number): void {
    backlog.entries.length = 0;
    backlog.byteLength = 0;
    this.#appendBacklog(backlog, event, byteLength);
  }

  #removeBacklogKind(backlog: PaneBacklog, kind: PaneEvent["kind"]): void {
    let write = 0;
    for (let read = 0; read < backlog.entries.length; read += 1) {
      const entry = backlog.entries[read];
      if (entry.event.kind === kind) {
        backlog.byteLength -= entry.byteLength;
      } else {
        backlog.entries[write++] = entry;
      }
    }
    backlog.entries.length = write;
  }

  #setBacklog(pane: PaneStreamState, backlog: PaneBacklog): void {
    const previous = pane.backlog;
    if (previous) this.#backlogBytes -= previous.byteLength;
    else this.#backlogCount += 1;
    pane.backlog = backlog;
    this.#backlogBytes += backlog.byteLength;
    this.measurements?.highWater?.("terminal.hub.retainedBytes", this.#backlogBytes);
    this.measurements?.highWater?.("terminal.hub.retainedPanes", this.#backlogCount);
  }

  #deleteBacklog(pane: PaneStreamState): void {
    if (!pane.backlog) return;
    this.#backlogBytes -= pane.backlog.byteLength;
    this.#backlogCount -= 1;
    pane.backlog = undefined;
  }

  #takeBacklog(pane: PaneStreamState): PaneBacklog | undefined {
    const backlog = pane.backlog;
    if (!backlog) return undefined;
    this.#backlogBytes -= backlog.byteLength;
    this.#backlogCount -= 1;
    pane.backlog = undefined;
    return backlog;
  }

  #enforceBacklogLimits(): void {
    while (this.#backlogCount > this.#maxBufferedPanes || this.#backlogBytes > this.#maxTotalBytes) {
      const oldest = this.#oldestBufferedPane();
      if (oldest === undefined) break;
      this.#requireSeed(oldest, this.#backlogBytes > this.#maxTotalBytes
        ? "frontend aggregate hidden recovery budget was exceeded"
        : "frontend hidden-pane LRU capacity was exceeded");
    }
  }

  #requireSeed(paneId: string, reason: string): void {
    const pane = this.#paneStates.get(paneId) ?? this.#touchPane(paneId);
    const alreadyAwaiting = pane.awaitingSeed;
    pane.awaitingSeed = true;
    this.#deleteBacklog(pane);
    if (!alreadyAwaiting) this.onSeedRequired?.(paneId, reason);
  }

  #touchPane(paneId: string): PaneStreamState {
    const pane = this.#paneStates.get(paneId) ?? createPaneStreamState();
    this.#paneStates.delete(paneId);
    this.#paneStates.set(paneId, pane);
    while (this.#paneStates.size > this.#maxTrackedPanes) {
      let oldest: string | undefined;
      for (const candidate of this.#paneStates.keys()) {
        if (candidate !== paneId && !this.#paneListeners.has(candidate)) {
          oldest = candidate;
          break;
        }
      }
      if (oldest === undefined) break;
      const evicted = this.#paneStates.get(oldest);
      this.#paneStates.delete(oldest);
      if (evicted) {
        const alreadyAwaiting = evicted.awaitingSeed;
        this.#deleteBacklog(evicted);
        this.#rememberEvictedSeedDebt(oldest);
        if (!alreadyAwaiting) this.onSeedRequired?.(oldest, "frontend pane metadata LRU capacity was exceeded");
      }
    }
    return pane;
  }

  #oldestBufferedPane(): string | undefined {
    for (const [paneId, pane] of this.#paneStates) {
      if (pane.backlog) return paneId;
    }
    return undefined;
  }

  #rememberEvictedSeedDebt(paneId: string): void {
    this.#evictedSeedDebt.delete(paneId);
    this.#evictedSeedDebt.set(paneId, true);
    while (this.#evictedSeedDebt.size > this.#maxTrackedPanes) {
      const oldest = this.#evictedSeedDebt.keys().next().value as string | undefined;
      if (oldest === undefined) break;
      this.#evictedSeedDebt.delete(oldest);
      // A single bounded sentinel is sufficient once the exact tombstone is
      // gone: any later untracked pane must establish itself with a seed.
      this.#unknownPanesRequireSeed = true;
    }
  }

  #admitSequence(event: TerminalEvent): TerminalEventAdmission {
    if (event.sequence === 0) {
      if (event.kind === "generationEpoch" && event.epoch !== this.#generationEpoch) {
        this.#lastSequence = 0;
        this.#sequenceFrozen = false;
      }
      return { kind: "local" };
    }
    if (event.kind === "snapshot" && event.authoritative) {
      this.#lastSequence = event.sequence;
      this.#sequenceFrozen = false;
      return { kind: "accepted" };
    }
    if (this.#sequenceFrozen) return { kind: "gap", expected: this.#lastSequence + 1, received: event.sequence };
    if (event.sequence <= this.#lastSequence) return { kind: "stale" };
    const expected = this.#lastSequence + 1;
    if (event.sequence !== expected) {
      this.#sequenceFrozen = true;
      return { kind: "gap", expected, received: event.sequence };
    }
    this.#lastSequence = event.sequence;
    return { kind: "accepted" };
  }

  #requestConflictReseed(paneId: string, pane: PaneStreamState): void {
    if (pane.conflictReseedRequested) return;
    pane.conflictReseedRequested = true;
    this.onSeedRequired?.(paneId, "stale or conflicting terminal visibility handoff");
  }
}

function samePaneResource(left: PaneResourceIdentity, right: PaneResourceIdentity): boolean {
  return left.state === right.state
    && left.requiresSeed === right.requiresSeed
    && left.generation === right.generation
    && left.snapshotGeneration === right.snapshotGeneration
    && left.tailThroughGeneration === right.tailThroughGeneration
    && left.recoveryReason === right.recoveryReason
    && sameBytes(left.serializedSnapshot, right.serializedSnapshot)
    && sameBytes(left.rawTail, right.rawTail);
}

function copyPaneResourceIdentity(event: Extract<PaneEvent, { kind: "paneResource" }>): PaneResourceIdentity | undefined {
  const identityBytes = diagnosticEncoder.encode(event.recoveryReason).byteLength
    + event.serializedSnapshot.byteLength
    + event.rawTail.byteLength;
  // Exact duplicate recognition is an optimization, not an authority check.
  // Keep it bounded; oversized same-generation checkpoints conservatively
  // request a new seed instead of relying on a collision-prone digest.
  if (identityBytes > MAX_EXACT_RESOURCE_IDENTITY_BYTES) return undefined;
  return {
    state: event.state,
    requiresSeed: event.requiresSeed,
    generation: event.generation,
    snapshotGeneration: event.snapshotGeneration,
    tailThroughGeneration: event.tailThroughGeneration,
    recoveryReason: event.recoveryReason,
    serializedSnapshot: event.serializedSnapshot.slice(),
    rawTail: event.rawTail.slice(),
  };
}

function sameBytes(left: Uint8Array, right: Uint8Array): boolean {
  if (left.byteLength !== right.byteLength) return false;
  for (let index = 0; index < left.byteLength; index += 1) {
    if (left[index] !== right[index]) return false;
  }
  return true;
}
