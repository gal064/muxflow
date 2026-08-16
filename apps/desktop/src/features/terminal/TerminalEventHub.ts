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
 * Routes pane events without retaining an unbounded all-host history. Active
 * consumers own a separate registry; a Map's insertion order is exclusively
 * the dormant-pane LRU, so output delivery never scans mounted panes.
 */
export class TerminalEventHub {
  readonly #epochListeners = new Set<EpochListener>();
  readonly #paneListeners = new Map<string, PaneListener>();
  readonly #activePaneStates = new Map<string, PaneStreamState>();
  readonly #dormantPaneStates = new Map<string, PaneStreamState>();
  readonly #evictedSeedDebt = new Map<string, true>();
  readonly #maxPaneBytes: number;
  readonly #maxTotalBytes: number;
  readonly #maxBufferedPanes: number;
  readonly #maxTrackedPanes: number;
  readonly #maxPaneEvents: number;
  #backlogBytes = 0;
  #backlogCount = 0;
  #retainedPaneCount = 0;
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
    const wasTracked = this.#activePaneStates.has(event.paneId) || this.#dormantPaneStates.has(event.paneId);
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
        if (event.generation <= lastGeneration) {
          const previous = latestBufferedPaneResource(pane.backlog);
          if (event.generation < lastGeneration || !previous || !samePaneResource(previous, event)) {
            this.#requestConflictReseed(event.paneId, pane);
          }
          return admission;
        }
        pane.lastGeneration = event.generation;
      } else if (event.generation <= lastGeneration) {
        // A seed is authoritative content, not an increment: dropping one
        // because its generation looks stale leaves the pane waiting for a
        // screen that has already been sent and will not be sent again
        // (P12-U003.3). Ask for one that this hub can accept instead.
        if (event.kind === "seed") this.#requestConflictReseed(event.paneId, pane);
        return admission;
      } else {
        pane.lastGeneration = event.generation;
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
      try {
        listener(event);
      } catch (error) {
        // Admission advances the pane generation before delivery. If the sole
        // renderer rejects that transfer, the hub can no longer prove which
        // prefix reached xterm, so incremental traffic must wait for a seed.
        this.#requireSeed(event.paneId, "terminal pane consumer rejected an admitted event");
        throw error;
      }
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
    const pane = this.#dormantPaneStates.get(paneId) ?? createPaneStreamState();
    this.#dormantPaneStates.delete(paneId);
    this.#activePaneStates.set(paneId, pane);
    this.#paneListeners.set(paneId, listener);
    const backlog = pane?.backlog;
    if (backlog) {
      this.#deleteBacklog(pane);
      try {
        for (const entry of backlog.entries) {
          this.measurements?.add("terminal.hub.fanoutDeliveries");
          listener(entry.event);
        }
      } catch (error) {
        // The caller never received an unsubscribe handle, and some prefix of
        // the backlog may already have moved to the renderer. Roll ownership
        // back before surfacing the exception and require one authoritative
        // replacement instead of replaying an ambiguous suffix.
        this.#paneListeners.delete(paneId);
        this.#activePaneStates.delete(paneId);
        const alreadyAwaiting = pane.awaitingSeed;
        pane.awaitingSeed = true;
        pane.conflictReseedRequested = false;
        this.#retainDormantPane(paneId, pane);
        if (!alreadyAwaiting) {
          this.onSeedRequired?.(paneId, "terminal pane consumer rejected backlog replay");
        }
        throw error;
      }
      this.measurements?.add("terminal.hub.backlogDequeues", backlog.entries.length);
    }
    return () => {
      if (this.#paneListeners.get(paneId) !== listener) return;
      this.#paneListeners.delete(paneId);
      const active = this.#activePaneStates.get(paneId);
      this.#activePaneStates.delete(paneId);
      if (active) this.#retainDormantPane(paneId, active);
    };
  }

  clearPane(paneId: string): void {
    const pane = this.#activePaneStates.get(paneId) ?? this.#dormantPaneStates.get(paneId);
    if (pane) {
      this.#deleteBacklog(pane);
    }
    this.#dormantPaneStates.delete(paneId);
    if (this.#paneListeners.has(paneId)) {
      this.#activePaneStates.set(paneId, createPaneStreamState());
    } else {
      this.#activePaneStates.delete(paneId);
    }
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
      outputGeneration: (this.#activePaneStates.get(paneId) ?? this.#dormantPaneStates.get(paneId))?.renderedGeneration ?? 0,
    };
  }

  get generationEpoch(): number | undefined {
    return this.#generationEpoch;
  }

  get lastSequence(): number {
    return this.#lastSequence;
  }

  get retainedPaneCount(): number {
    return this.#retainedPaneCount;
  }

  get retainedByteLength(): number {
    return this.#backlogBytes;
  }

  get trackedPaneCount(): number {
    return this.#activePaneStates.size + this.#dormantPaneStates.size;
  }

  #clearPaneState(): void {
    this.#dormantPaneStates.clear();
    this.#activePaneStates.clear();
    for (const paneId of this.#paneListeners.keys()) {
      this.#activePaneStates.set(paneId, createPaneStreamState());
    }
    this.#backlogBytes = 0;
    this.#backlogCount = 0;
    this.#retainedPaneCount = 0;
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
    const wasRetained = this.#paneRetainsBytes(pane);
    const previous = pane.backlog;
    if (previous) this.#backlogBytes -= previous.byteLength;
    else this.#backlogCount += 1;
    pane.backlog = backlog;
    this.#backlogBytes += backlog.byteLength;
    this.#finishRetentionMutation(pane, wasRetained);
  }

  #deleteBacklog(pane: PaneStreamState): void {
    if (!pane.backlog) return;
    const wasRetained = this.#paneRetainsBytes(pane);
    this.#backlogBytes -= pane.backlog.byteLength;
    this.#backlogCount -= 1;
    pane.backlog = undefined;
    this.#finishRetentionMutation(pane, wasRetained);
  }

  #takeBacklog(pane: PaneStreamState): PaneBacklog | undefined {
    const backlog = pane.backlog;
    if (!backlog) return undefined;
    const wasRetained = this.#paneRetainsBytes(pane);
    this.#backlogBytes -= backlog.byteLength;
    this.#backlogCount -= 1;
    pane.backlog = undefined;
    this.#finishRetentionMutation(pane, wasRetained);
    return backlog;
  }

  #paneRetainsBytes(pane: PaneStreamState): boolean {
    return pane.backlog !== undefined;
  }

  #finishRetentionMutation(pane: PaneStreamState, wasRetained: boolean): void {
    const isRetained = this.#paneRetainsBytes(pane);
    if (wasRetained !== isRetained) this.#retainedPaneCount += isRetained ? 1 : -1;
    this.measurements?.highWater?.(
      "terminal.hub.retainedBytes",
      this.#backlogBytes,
    );
    this.measurements?.highWater?.("terminal.hub.retainedPanes", this.#retainedPaneCount);
  }

  #enforceBacklogLimits(): void {
    while (this.#backlogCount > this.#maxBufferedPanes
      || this.#backlogBytes > this.#maxTotalBytes) {
      const oldest = this.#oldestBufferedPane();
      if (oldest === undefined) break;
      this.#requireSeed(oldest, this.#backlogBytes > this.#maxTotalBytes
        ? "frontend aggregate hidden recovery budget was exceeded"
        : "frontend hidden-pane LRU capacity was exceeded");
    }
  }

  #requireSeed(paneId: string, reason: string): void {
    const pane = this.#activePaneStates.get(paneId) ?? this.#dormantPaneStates.get(paneId) ?? this.#touchPane(paneId);
    const alreadyAwaiting = pane.awaitingSeed;
    pane.awaitingSeed = true;
    this.#deleteBacklog(pane);
    if (!alreadyAwaiting) this.onSeedRequired?.(paneId, reason);
  }

  #touchPane(paneId: string): PaneStreamState {
    const active = this.#activePaneStates.get(paneId);
    if (active) return active;
    const pane = this.#dormantPaneStates.get(paneId) ?? createPaneStreamState();
    this.#dormantPaneStates.delete(paneId);
    this.#dormantPaneStates.set(paneId, pane);
    this.#enforceDormantPaneLimit();
    return pane;
  }

  #retainDormantPane(paneId: string, pane: PaneStreamState): void {
    this.#dormantPaneStates.delete(paneId);
    this.#dormantPaneStates.set(paneId, pane);
    this.#enforceDormantPaneLimit();
  }

  #enforceDormantPaneLimit(): void {
    while (this.#dormantPaneStates.size > this.#maxTrackedPanes) {
      const oldest = this.#dormantPaneStates.keys().next().value as string | undefined;
      if (oldest === undefined) break;
      const evicted = this.#dormantPaneStates.get(oldest);
      this.#dormantPaneStates.delete(oldest);
      if (evicted) {
        const alreadyAwaiting = evicted.awaitingSeed;
        this.#deleteBacklog(evicted);
        this.#rememberEvictedSeedDebt(oldest);
        if (!alreadyAwaiting) this.onSeedRequired?.(oldest, "frontend pane metadata LRU capacity was exceeded");
      }
    }
  }

  #oldestBufferedPane(): string | undefined {
    for (const [paneId, pane] of this.#dormantPaneStates) {
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

function latestBufferedPaneResource(backlog: PaneBacklog | undefined): Extract<PaneEvent, { kind: "paneResource" }> | undefined {
  for (let index = (backlog?.entries.length ?? 0) - 1; index >= 0; index -= 1) {
    const event = backlog!.entries[index].event;
    if (event.kind === "paneResource") return event;
  }
  return undefined;
}

function samePaneResource(
  left: Extract<PaneEvent, { kind: "paneResource" }>,
  right: Extract<PaneEvent, { kind: "paneResource" }>,
): boolean {
  const identityBytes = diagnosticEncoder.encode(right.recoveryReason).byteLength
    + right.serializedSnapshot.byteLength + right.rawTail.byteLength;
  if (identityBytes > MAX_EXACT_RESOURCE_IDENTITY_BYTES) return false;
  return left.state === right.state
    && left.requiresSeed === right.requiresSeed
    && left.generation === right.generation
    && left.snapshotGeneration === right.snapshotGeneration
    && left.tailThroughGeneration === right.tailThroughGeneration
    && left.recoveryReason === right.recoveryReason
    && sameBytes(left.serializedSnapshot, right.serializedSnapshot)
    && sameBytes(left.rawTail, right.rawTail);
}

function sameBytes(left: Uint8Array, right: Uint8Array): boolean {
  if (left.byteLength !== right.byteLength) return false;
  for (let index = 0; index < left.byteLength; index += 1) {
    if (left[index] !== right[index]) return false;
  }
  return true;
}
