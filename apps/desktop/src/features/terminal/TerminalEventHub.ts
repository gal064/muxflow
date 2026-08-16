import type { TerminalEvent } from "./api";

type Listener = (event: TerminalEvent) => void;
type PaneEvent = Extract<TerminalEvent, { kind: "seed" | "output" | "paneResource" | "seedDiagnostic" }>;

const DEFAULT_MAX_PANE_BYTES = 8 * 1024 * 1024;
const DEFAULT_MAX_TOTAL_BYTES = 16 * 1024 * 1024;
const DEFAULT_MAX_BUFFERED_PANES = 32;
const diagnosticEncoder = new TextEncoder();

interface PaneBacklog {
  events: PaneEvent[];
  byteLength: number;
}

export interface TerminalEventHubLimits {
  maxPaneBytes?: number;
  maxTotalBytes?: number;
  maxBufferedPanes?: number;
  maxTrackedPanes?: number;
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
  #backlogBytes = 0;
  #generationEpoch?: number;
  #lastSequence = 0;
  #sequenceFrozen = false;

  constructor(
    readonly onSeedRequired?: (paneId: string, reason: string) => void,
    limits: TerminalEventHubLimits = {},
  ) {
    this.#maxPaneBytes = limits.maxPaneBytes ?? DEFAULT_MAX_PANE_BYTES;
    this.#maxTotalBytes = limits.maxTotalBytes ?? DEFAULT_MAX_TOTAL_BYTES;
    this.#maxBufferedPanes = limits.maxBufferedPanes ?? DEFAULT_MAX_BUFFERED_PANES;
    this.#maxTrackedPanes = limits.maxTrackedPanes ?? Math.max(64, this.#maxBufferedPanes * 4);
  }

  publish(event: TerminalEvent, beforeDelivery?: () => void): TerminalEventAdmission {
    const admission = this.#admitSequence(event);
    if (admission.kind === "stale" || admission.kind === "gap") return admission;
    if (event.kind === "generationEpoch" && event.epoch !== this.#generationEpoch) {
      this.#generationEpoch = event.epoch;
      this.#clearPaneState();
    }
    beforeDelivery?.();
    for (const listener of this.#listeners) listener(event);
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
      for (const listener of listeners) listener(event);
      return admission;
    }
    this.#buffer(event);
    return admission;
  }

  subscribe(listener: Listener): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  subscribePane(paneId: string, listener: Listener): () => void {
    const listeners = this.#paneListeners.get(paneId) ?? new Set<Listener>();
    listeners.add(listener);
    this.#paneListeners.set(paneId, listeners);
    const backlog = this.#backlogs.get(paneId);
    if (backlog) {
      this.#deleteBacklog(paneId);
      for (const event of backlog.events) listener(event);
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
    const current = this.#backlogs.get(event.paneId) ?? { events: [], byteLength: 0 };
    let next: PaneBacklog;
    if (event.kind === "seed") {
      next = { events: [event], byteLength: event.data.byteLength };
    } else if (event.kind === "paneResource") {
      const resourceBytes = event.serializedSnapshot.byteLength + event.rawTail.byteLength;
      if (resourceBytes > 0) {
        next = { events: [event], byteLength: resourceBytes };
      } else {
        next = {
          events: [...current.events.filter((item) => item.kind !== "paneResource"), event],
          byteLength: current.byteLength,
        };
      }
    } else if (event.kind === "seedDiagnostic") {
      const withoutOldDiagnostic = current.events.filter((item) => item.kind !== "seedDiagnostic");
      const oldDiagnosticBytes = current.events
        .filter((item): item is Extract<PaneEvent, { kind: "seedDiagnostic" }> => item.kind === "seedDiagnostic")
        .reduce((total, item) => total + diagnosticEncoder.encode(item.message).byteLength, 0);
      const diagnosticBytes = diagnosticEncoder.encode(event.message).byteLength;
      next = {
        events: [...withoutOldDiagnostic, event],
        byteLength: current.byteLength - oldDiagnosticBytes + diagnosticBytes,
      };
    } else {
      next = { events: [...current.events, event], byteLength: current.byteLength + event.data.byteLength };
    }

    if (next.byteLength > this.#maxPaneBytes) {
      const limit = this.#maxPaneBytes === DEFAULT_MAX_PANE_BYTES ? "8 MiB" : `${this.#maxPaneBytes} bytes`;
      this.#requireSeed(event.paneId, `frontend hidden recovery buffer exceeded ${limit}`);
      return;
    }
    this.#setBacklog(event.paneId, next);
    this.#enforceBacklogLimits();
  }

  #setBacklog(paneId: string, backlog: PaneBacklog): void {
    const previous = this.#backlogs.get(paneId);
    if (previous) this.#backlogBytes -= previous.byteLength;
    this.#backlogs.delete(paneId);
    this.#backlogs.set(paneId, backlog);
    this.#backlogBytes += backlog.byteLength;
  }

  #deleteBacklog(paneId: string): void {
    const previous = this.#backlogs.get(paneId);
    if (previous) this.#backlogBytes -= previous.byteLength;
    this.#backlogs.delete(paneId);
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
