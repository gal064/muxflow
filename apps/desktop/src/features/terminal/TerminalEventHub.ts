import type { TerminalEvent } from "./api";
import type { OperationRecorder } from "../../perf/operations";

type Listener = (event: TerminalEvent) => void;
type EpochEvent = Extract<TerminalEvent, { kind: "generationEpoch" }>;
type EpochListener = (event: EpochEvent) => void;
type PaneEvent = Extract<TerminalEvent, { kind: "seed" | "output" | "paneResource" | "seedDiagnostic" }>;

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
  head: number;
  byteLength: number;
}

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
  readonly #listeners = new Set<Listener>();
  readonly #epochListeners = new Set<EpochListener>();
  readonly #paneListeners = new Map<string, Set<Listener>>();
  readonly #backlogs = new Map<string, PaneBacklog>();
  readonly #lastGeneration = new Map<string, number>();
  readonly #lastPaneResource = new Map<string, Extract<PaneEvent, { kind: "paneResource" }>>();
  readonly #renderedGeneration = new Map<string, number>();
  readonly #awaitingSeed = new Set<string>();
  readonly #conflictReseedRequested = new Set<string>();
  readonly #trackedPaneLru = new Map<string, true>();
  readonly #maxPaneBytes: number;
  readonly #maxTotalBytes: number;
  readonly #maxBufferedPanes: number;
  readonly #maxTrackedPanes: number;
  readonly #maxPaneEvents: number;
  #backlogBytes = 0;
  #generationEpoch?: number;
  #lastSequence = 0;
  #sequenceFrozen = false;

  constructor(
    readonly onSeedRequired?: (paneId: string, reason: string) => void,
    limits: TerminalEventHubLimits = {},
    readonly measurements?: OperationRecorder,
  ) {
    this.#maxPaneBytes = limits.maxPaneBytes ?? DEFAULT_MAX_PANE_BYTES;
    this.#maxTotalBytes = limits.maxTotalBytes ?? DEFAULT_MAX_TOTAL_BYTES;
    this.#maxBufferedPanes = limits.maxBufferedPanes ?? DEFAULT_MAX_BUFFERED_PANES;
    this.#maxTrackedPanes = limits.maxTrackedPanes ?? Math.max(64, this.#maxBufferedPanes * 4);
    this.#maxPaneEvents = limits.maxPaneEvents ?? DEFAULT_MAX_PANE_EVENTS;
  }

  publish(event: TerminalEvent, beforeDelivery?: () => void): TerminalEventAdmission {
    this.measurements?.add("terminal.hub.events");
    if (event.kind === "seed" || event.kind === "output") {
      this.measurements?.add("terminal.hub.payloadBytes", event.data.byteLength);
    }
    const admission = this.#admitSequence(event);
    if (admission.kind === "stale" || admission.kind === "gap") return admission;
    if (event.kind === "generationEpoch" && event.epoch !== this.#generationEpoch) {
      this.#generationEpoch = event.epoch;
      this.#clearPaneState();
    }
    beforeDelivery?.();
    if (event.kind === "generationEpoch") {
      for (const listener of this.#epochListeners) {
        this.measurements?.add("terminal.hub.epochDeliveries");
        listener(event);
      }
    }
    for (const listener of this.#listeners) {
      this.measurements?.add("terminal.hub.fanoutDeliveries");
      listener(event);
    }
    if (event.kind === "generationEpoch") return admission;
    if (event.kind !== "seed" && event.kind !== "output" && event.kind !== "paneResource" && event.kind !== "seedDiagnostic") return admission;
    this.#touchTrackedPane(event.paneId);
    if (event.kind !== "seedDiagnostic") {
      const lastGeneration = this.#lastGeneration.get(event.paneId) ?? -1;
      if (event.kind === "paneResource" && event.generation <= lastGeneration) {
        const previous = this.#lastPaneResource.get(event.paneId);
        if (event.generation < lastGeneration || !previous || !samePaneResource(previous, event)) {
          this.#requestConflictReseed(event.paneId);
        }
        return admission;
      }
      if (event.generation <= lastGeneration) {
        // A seed is authoritative content, not an increment: dropping one
        // because its generation looks stale leaves the pane waiting for a
        // screen that has already been sent and will not be sent again
        // (P12-U003.3). Ask for one that this hub can accept instead.
        if (event.kind === "seed") this.#requestConflictReseed(event.paneId);
        return admission;
      }
      this.#lastGeneration.set(event.paneId, event.generation);
      if (event.kind === "paneResource") this.#lastPaneResource.set(event.paneId, event);
      else this.#lastPaneResource.delete(event.paneId);
      if (event.kind === "paneResource" && event.requiresSeed) {
        this.#awaitingSeed.add(event.paneId);
        this.#deleteBacklog(event.paneId);
      } else if (event.kind === "seed") {
        this.#awaitingSeed.delete(event.paneId);
        this.#conflictReseedRequested.delete(event.paneId);
      } else if (event.kind === "paneResource") {
        // Recovery material is an authoritative replacement for a locally
        // evicted backlog. An empty reveal does not cancel a pending seed.
        if (event.serializedSnapshot.byteLength + event.rawTail.byteLength > 0) {
          this.#awaitingSeed.delete(event.paneId);
          this.#conflictReseedRequested.delete(event.paneId);
        } else if (this.#awaitingSeed.has(event.paneId)) {
          return admission;
        }
      } else if (this.#awaitingSeed.has(event.paneId)) {
        return admission;
      }
    }
    const listeners = this.#paneListeners.get(event.paneId);
    if (listeners?.size) {
      for (const listener of listeners) {
        this.measurements?.add("terminal.hub.fanoutDeliveries");
        listener(event);
      }
      return admission;
    }
    this.#buffer(event);
    return admission;
  }

  subscribe(listener: Listener): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
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

  subscribePane(paneId: string, listener: Listener): () => void {
    const listeners = this.#paneListeners.get(paneId) ?? new Set<Listener>();
    listeners.add(listener);
    this.#paneListeners.set(paneId, listeners);
    const backlog = this.#backlogs.get(paneId);
    if (backlog) {
      this.#deleteBacklog(paneId);
      let delivered = 0;
      while (backlog.head < backlog.entries.length) {
        this.measurements?.add("terminal.hub.fanoutDeliveries");
        listener(backlog.entries[backlog.head++].event);
        delivered += 1;
      }
      this.measurements?.add("terminal.hub.backlogDequeues", delivered);
    }
    return () => {
      listeners.delete(listener);
      if (listeners.size === 0) this.#paneListeners.delete(paneId);
    };
  }

  clearPane(paneId: string): void {
    this.#deleteBacklog(paneId);
    this.#lastGeneration.delete(paneId);
    this.#lastPaneResource.delete(paneId);
    this.#renderedGeneration.delete(paneId);
    this.#awaitingSeed.delete(paneId);
    this.#conflictReseedRequested.delete(paneId);
    this.#trackedPaneLru.delete(paneId);
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
    const current = this.#renderedGeneration.get(paneId) ?? 0;
    if (generation > current) this.#renderedGeneration.set(paneId, generation);
  }

  visibilityCheckpoint(paneId: string): { terminalEpoch: number; outputGeneration: number } | undefined {
    if (this.#generationEpoch === undefined) return undefined;
    return {
      terminalEpoch: this.#generationEpoch,
      outputGeneration: this.#renderedGeneration.get(paneId) ?? 0,
    };
  }

  get generationEpoch(): number | undefined {
    return this.#generationEpoch;
  }

  get lastSequence(): number {
    return this.#lastSequence;
  }

  get retainedPaneCount(): number {
    return this.#backlogs.size;
  }

  get retainedByteLength(): number {
    return this.#backlogBytes;
  }

  get trackedPaneCount(): number {
    return this.#trackedPaneLru.size;
  }

  #clearPaneState(): void {
    this.#backlogs.clear();
    this.#backlogBytes = 0;
    this.#lastGeneration.clear();
    this.#lastPaneResource.clear();
    this.#renderedGeneration.clear();
    this.#awaitingSeed.clear();
    this.#conflictReseedRequested.clear();
    this.#trackedPaneLru.clear();
  }

  #buffer(event: PaneEvent): void {
    this.measurements?.add("terminal.hub.backlogEnqueues");
    // Detach before mutating so aggregate accounting always describes the
    // bytes actually retained in the map, including overflow/reseed paths.
    const current = this.#takeBacklog(event.paneId) ?? { entries: [], head: 0, byteLength: 0 };
    if (event.kind === "seed") {
      this.#replaceBacklog(current, event, event.data.byteLength);
    } else if (event.kind === "paneResource") {
      const resourceBytes = event.serializedSnapshot.byteLength + event.rawTail.byteLength;
      if (resourceBytes > 0) {
        this.#replaceBacklog(current, event, resourceBytes);
      } else {
        this.#removeBacklogKind(current, "paneResource");
        this.#appendBacklog(current, event, 0);
      }
    } else if (event.kind === "seedDiagnostic") {
      this.#removeBacklogKind(current, "seedDiagnostic");
      this.#appendBacklog(current, event, diagnosticEncoder.encode(event.message).byteLength);
    } else {
      this.#appendBacklog(current, event, event.data.byteLength);
    }

    if (current.byteLength > this.#maxPaneBytes || this.#backlogLength(current) > this.#maxPaneEvents) {
      const limit = this.#maxPaneBytes === DEFAULT_MAX_PANE_BYTES ? "8 MiB" : `${this.#maxPaneBytes} bytes`;
      this.#requireSeed(
        event.paneId,
        current.byteLength > this.#maxPaneBytes
          ? `frontend hidden recovery buffer exceeded ${limit}`
          : `frontend hidden recovery record capacity exceeded ${this.#maxPaneEvents} events`,
      );
      return;
    }
    this.#setBacklog(event.paneId, current);
    this.#enforceBacklogLimits();
  }

  #appendBacklog(backlog: PaneBacklog, event: PaneEvent, byteLength: number): void {
    backlog.entries.push({ event, byteLength });
    backlog.byteLength += byteLength;
  }

  #replaceBacklog(backlog: PaneBacklog, event: PaneEvent, byteLength: number): void {
    backlog.head = backlog.entries.length;
    backlog.byteLength = 0;
    this.#compactBacklog(backlog);
    this.#appendBacklog(backlog, event, byteLength);
  }

  #removeBacklogKind(backlog: PaneBacklog, kind: PaneEvent["kind"]): void {
    let write = 0;
    for (let read = backlog.head; read < backlog.entries.length; read += 1) {
      const entry = backlog.entries[read];
      if (entry.event.kind === kind) {
        backlog.byteLength -= entry.byteLength;
      } else {
        backlog.entries[write++] = entry;
      }
    }
    backlog.entries.length = write;
    backlog.head = 0;
  }

  #compactBacklog(backlog: PaneBacklog): void {
    if (backlog.head === 0) return;
    if (backlog.head === backlog.entries.length) {
      backlog.entries.length = 0;
      backlog.head = 0;
      return;
    }
    // Replacement events advance the head in O(1). Compact only after enough
    // dead entries accrue, keeping retained array capacity bounded.
    if (backlog.head < 64 || backlog.head * 2 < backlog.entries.length) return;
    backlog.entries.copyWithin(0, backlog.head);
    backlog.entries.length -= backlog.head;
    backlog.head = 0;
    this.measurements?.add("terminal.hub.backlogCompactions");
  }

  #backlogLength(backlog: PaneBacklog): number {
    return backlog.entries.length - backlog.head;
  }

  #setBacklog(paneId: string, backlog: PaneBacklog): void {
    const previous = this.#backlogs.get(paneId);
    if (previous) this.#backlogBytes -= previous.byteLength;
    this.#backlogs.delete(paneId);
    this.#backlogs.set(paneId, backlog);
    this.#backlogBytes += backlog.byteLength;
    this.measurements?.highWater?.("terminal.hub.retainedBytes", this.#backlogBytes);
    this.measurements?.highWater?.("terminal.hub.retainedPanes", this.#backlogs.size);
  }

  #deleteBacklog(paneId: string): void {
    const previous = this.#backlogs.get(paneId);
    if (previous) this.#backlogBytes -= previous.byteLength;
    this.#backlogs.delete(paneId);
  }

  #takeBacklog(paneId: string): PaneBacklog | undefined {
    const backlog = this.#backlogs.get(paneId);
    if (!backlog) return undefined;
    this.#backlogBytes -= backlog.byteLength;
    this.#backlogs.delete(paneId);
    return backlog;
  }

  #enforceBacklogLimits(): void {
    while (this.#backlogs.size > this.#maxBufferedPanes || this.#backlogBytes > this.#maxTotalBytes) {
      const oldest = this.#backlogs.keys().next().value as string | undefined;
      if (oldest === undefined) break;
      this.#requireSeed(oldest, this.#backlogBytes > this.#maxTotalBytes
        ? "frontend aggregate hidden recovery budget was exceeded"
        : "frontend hidden-pane LRU capacity was exceeded");
    }
  }

  #requireSeed(paneId: string, reason: string): void {
    const alreadyAwaiting = this.#awaitingSeed.has(paneId);
    this.#awaitingSeed.add(paneId);
    this.#deleteBacklog(paneId);
    if (!alreadyAwaiting) this.onSeedRequired?.(paneId, reason);
  }

  #touchTrackedPane(paneId: string): void {
    this.#trackedPaneLru.delete(paneId);
    this.#trackedPaneLru.set(paneId, true);
    while (this.#trackedPaneLru.size > this.#maxTrackedPanes) {
      const oldest = this.#trackedPaneLru.keys().next().value as string | undefined;
      if (oldest === undefined) break;
      this.#trackedPaneLru.delete(oldest);
      this.#lastGeneration.delete(oldest);
      this.#lastPaneResource.delete(oldest);
      this.#renderedGeneration.delete(oldest);
      if (this.#backlogs.has(oldest)) {
        const alreadyAwaiting = this.#awaitingSeed.has(oldest);
        this.#deleteBacklog(oldest);
        if (!alreadyAwaiting) this.onSeedRequired?.(oldest, "frontend pane metadata LRU capacity was exceeded");
      }
      this.#awaitingSeed.delete(oldest);
      this.#conflictReseedRequested.delete(oldest);
    }
  }

  #admitSequence(event: TerminalEvent): TerminalEventAdmission {
    if (event.sequence === 0) {
      if (event.kind === "generationEpoch") {
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

  #requestConflictReseed(paneId: string): void {
    if (this.#conflictReseedRequested.has(paneId)) return;
    this.#conflictReseedRequested.add(paneId);
    this.onSeedRequired?.(paneId, "stale or conflicting terminal visibility handoff");
  }
}

function samePaneResource(
  left: Extract<PaneEvent, { kind: "paneResource" }>,
  right: Extract<PaneEvent, { kind: "paneResource" }>,
): boolean {
  return left.state === right.state
    && left.requiresSeed === right.requiresSeed
    && left.recoveryReason === right.recoveryReason
    && left.snapshotGeneration === right.snapshotGeneration
    && left.tailThroughGeneration === right.tailThroughGeneration
    && equalBytes(left.serializedSnapshot, right.serializedSnapshot)
    && equalBytes(left.rawTail, right.rawTail);
}

function equalBytes(left: Uint8Array, right: Uint8Array): boolean {
  return left.byteLength === right.byteLength && left.every((byte, index) => byte === right[index]);
}
