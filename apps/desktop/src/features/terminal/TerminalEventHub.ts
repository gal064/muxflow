import type { TerminalEvent } from "./api";
import type { OperationRecorder } from "../../perf/operations";
import { recordIncident } from "../../diagnostics/incidents";

type EpochEvent = Extract<TerminalEvent, { kind: "generationEpoch" }>;
type EpochListener = (event: EpochEvent) => void;
type PaneEvent = Extract<TerminalEvent, { kind: "seed" | "output" | "paneResource" | "seedDiagnostic" }>;
type PaneListener = (event: PaneEvent) => void;

/**
 * The degraded states this hub holds a pane in.
 *
 * Both are one-shot latches whose only exit is an authoritative seed, and the
 * pane's own consumer cannot see either of them: while `awaitingSeed` stands the
 * hub drops that pane's events rather than delivering them, so the consumer is
 * told nothing at all. Exposing them is what lets the pane put a time bound on
 * a recovery signal that may never arrive.
 */
export interface PaneHealth {
  /** Output is being dropped until an authoritative seed re-establishes the pane. */
  awaitingSeed: boolean;
  /** A stale or conflicting handoff was rejected and one reseed was requested. */
  conflictReseedRequested: boolean;
}

type PaneHealthListener = (health: PaneHealth) => void;

function paneHealthOf(pane: PaneStreamState | undefined): PaneHealth {
  return {
    awaitingSeed: pane?.awaitingSeed ?? false,
    conflictReseedRequested: pane?.conflictReseedRequested ?? false,
  };
}

function samePaneHealth(left: PaneHealth, right: PaneHealth): boolean {
  return left.awaitingSeed === right.awaitingSeed
    && left.conflictReseedRequested === right.conflictReseedRequested;
}

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
  | { kind: "stale" };

/**
 * How often a sequence jump may cost every mounted pane a reseed.
 *
 * A jump is one anomaly however many frames it spans, and the frames arrive in
 * a burst: repairing per event would ask the host for the same screens dozens
 * of times for a single incident.
 */
const SEQUENCE_REPAIR_DEBOUNCE_MS = 1_000;

/**
 * Routes pane events without retaining an unbounded all-host history. Active
 * consumers own a separate registry; a Map's insertion order is exclusively
 * the dormant-pane LRU, so output delivery never scans mounted panes.
 */
export class TerminalEventHub {
  readonly #epochListeners = new Set<EpochListener>();
  readonly #paneListeners = new Map<string, PaneListener>();
  readonly #paneHealthListeners = new Map<string, PaneHealthListener>();
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
  #lastSequenceRepairAt = Number.NEGATIVE_INFINITY;
  #unknownPanesRequireSeed = false;

  constructor(
    readonly onSeedRequired?: (paneId: string, reason: string) => void,
    limits: TerminalEventHubLimits = {},
    readonly measurements?: OperationRecorder,
    readonly onObserverFailure?: (message: string) => void,
    /**
     * Journal-only notice that a pane's screen has just been repainted, used by
     * the echo-lag probe. It runs on the fanout path, so it must not throw:
     * a failure here would be indistinguishable from the pane's own consumer
     * rejecting the event.
     */
    readonly onPaneRepaint?: (paneId: string) => void,
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
    if (admission.kind === "stale") return admission;
    if (epochChanged && event.kind === "generationEpoch") {
      this.#generationEpoch = event.epoch;
      this.#clearPaneState();
    }
    if (event.kind !== "generationEpoch" || epochChanged) {
      try {
        beforeDelivery?.();
      } catch (error) {
        this.#reportObserverFailure("application delivery observer", error);
      }
    }
    if (epochChanged && event.kind === "generationEpoch") {
      for (const listener of this.#epochListeners) {
        this.measurements?.add("terminal.hub.epochDeliveries");
        try {
          listener(event);
        } catch (error) {
          this.#reportObserverFailure("terminal epoch observer", error);
        }
      }
    }
    if (event.kind === "generationEpoch") return admission;
    if (event.kind !== "seed" && event.kind !== "output" && event.kind !== "paneResource" && event.kind !== "seedDiagnostic") return admission;
    const wasTracked = this.#activePaneStates.has(event.paneId) || this.#dormantPaneStates.has(event.paneId);
    const hadEvictedSeedDebt = this.#evictedSeedDebt.delete(event.paneId);
    const requiresConservativeSeed = !wasTracked && this.#unknownPanesRequireSeed;
    const pane = this.#touchPane(event.paneId);
    // Everything below can latch this pane into a degraded state and then leave
    // through any of a dozen early returns. Compare the health once around the
    // whole thing so no exit can silently strand the pane's consumer.
    const healthBefore = paneHealthOf(pane);
    try {
      return this.#publishToPane(event, pane, admission, hadEvictedSeedDebt, requiresConservativeSeed);
    } finally {
      this.#notifyHealthChange(event.paneId, healthBefore);
    }
  }

  #publishToPane(
    event: PaneEvent,
    pane: PaneStreamState,
    admission: TerminalEventAdmission,
    hadEvictedSeedDebt: boolean,
    requiresConservativeSeed: boolean,
  ): TerminalEventAdmission {
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
          this.#requestSeed(event.paneId, "frontend pane recovery debt outlived the metadata LRU");
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
      // A seed or restored screen repaints the pane just as output does; a
      // diagnostic carries no content and repaints nothing.
      if (event.kind !== "seedDiagnostic") this.onPaneRepaint?.(event.paneId);
      this.measurements?.add("terminal.hub.fanoutDeliveries");
      try {
        listener(event);
      } catch {
        // Admission advances the pane generation before delivery. If the sole
        // renderer rejects that transfer, the hub can no longer prove which
        // prefix reached xterm, so incremental traffic must wait for a seed.
        // This is nevertheless a completed hub admission: rethrowing would
        // omit the wire frame from JS's cumulative delivery boundary while a
        // later frame could still be admitted in the same epoch. Consume this
        // frame after converting the pane to seed debt so native credit keeps
        // its exact ordinal/byte ownership.
        this.#requireSeed(event.paneId, "terminal pane consumer rejected an admitted event");
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

  /**
   * `onHealthChange` is how a mounted pane learns it has been latched into a
   * degraded state. It is called once with the pane's current health as part of
   * subscribing — a pane can mount straight into seed debt inherited from its
   * dormant state — and again on every later change.
   */
  subscribePane(paneId: string, listener: PaneListener, onHealthChange?: PaneHealthListener): () => void {
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
    if (onHealthChange) this.#paneHealthListeners.set(paneId, onHealthChange);
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
        this.#paneHealthListeners.delete(paneId);
        this.#activePaneStates.delete(paneId);
        const alreadyAwaiting = pane.awaitingSeed;
        pane.awaitingSeed = true;
        pane.conflictReseedRequested = false;
        this.#retainDormantPane(paneId, pane);
        if (!alreadyAwaiting) {
          this.#requestSeed(paneId, "terminal pane consumer rejected backlog replay");
        }
        throw error;
      }
      this.measurements?.add("terminal.hub.backlogDequeues", backlog.entries.length);
    }
    this.#notifyHealth(paneId);
    return () => {
      if (this.#paneListeners.get(paneId) !== listener) return;
      this.#paneListeners.delete(paneId);
      this.#paneHealthListeners.delete(paneId);
      const active = this.#activePaneStates.get(paneId);
      this.#activePaneStates.delete(paneId);
      if (active) this.#retainDormantPane(paneId, active);
    };
  }

  /** What this hub currently believes about a pane, mounted or not. */
  paneHealth(paneId: string): PaneHealth {
    return paneHealthOf(this.#activePaneStates.get(paneId) ?? this.#dormantPaneStates.get(paneId));
  }

  /**
   * Reopens the one-shot conflict-reseed latch.
   *
   * The hub asks for exactly one reseed per conflicting handoff, which is the
   * right rate limit and the wrong failure mode: if that seed never arrives, no
   * later conflict can ask again. The pane's watchdog calls this as part of
   * re-requesting the seed itself, so the latch bounds a *storm* rather than
   * bounding recovery to a single attempt. `awaitingSeed` is deliberately not
   * reopened: only an authoritative seed may clear that, or incremental output
   * would splice onto a screen the hub cannot prove.
   */
  retryPaneSeed(paneId: string): void {
    const pane = this.#activePaneStates.get(paneId) ?? this.#dormantPaneStates.get(paneId);
    if (!pane?.conflictReseedRequested) return;
    const healthBefore = paneHealthOf(pane);
    pane.conflictReseedRequested = false;
    this.#notifyHealthChange(paneId, healthBefore);
  }

  /**
   * Asks the host for one fresh screen per mounted pane.
   *
   * Repairs that keep the connection — a native in-place resync, a decoded
   * frame this process dropped — restore ordering without restoring content:
   * whatever those bytes were painting is simply missing from the panes. This
   * is the scoped answer to that, on the same request path the pane watchdogs
   * use, in place of the connection teardown that used to stand in for it.
   */
  reseedSubscribedPanes(reason: string): void {
    // Copied: a seed request runs application code that may mount or unmount a
    // pane, and the live map must not be iterated across that.
    for (const paneId of [...this.#paneListeners.keys()]) {
      const pane = this.#activePaneStates.get(paneId);
      if (pane?.conflictReseedRequested) {
        // The one-shot conflict latch bounds a storm of *handoff* conflicts. It
        // must not also swallow this reseed, which has its own cause, so reopen
        // it exactly as `retryPaneSeed` does before asking again.
        const healthBefore = paneHealthOf(pane);
        pane.conflictReseedRequested = false;
        this.#notifyHealthChange(paneId, healthBefore);
      }
      this.#requestSeed(paneId, reason);
    }
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
    this.#notifyHealth(paneId);
  }

  clear(): void {
    this.#generationEpoch = undefined;
    this.#lastSequence = 0;
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
    // Every mounted pane just had its seed debt discarded along with the state
    // that recorded it. A pane whose watchdog is armed on that debt has to hear
    // about it, or it keeps re-requesting a seed nobody owes it any more.
    for (const paneId of this.#paneHealthListeners.keys()) this.#notifyHealth(paneId);
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
    if (!alreadyAwaiting) this.#requestSeed(paneId, reason);
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
        if (!alreadyAwaiting) this.#requestSeed(oldest, "frontend pane metadata LRU capacity was exceeded");
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

  /**
   * Where the wire's ordering is read, and no longer where it is enforced.
   *
   * The native link owns sequence integrity: on a real break it stays on the
   * connection, requests a resync and forwards an authoritative snapshot at the
   * barrier. A hole reaching this far is therefore not a lost stretch of host
   * history — it is this process dropping a decoded frame — so freezing the
   * whole connection on it (which is what a `gap` admission used to do, via a
   * connection-epoch bump) tore down a link the native side had already
   * repaired. Admit the event, carry the watermark to it, and repair the only
   * thing actually at risk: the panes whose screens may have lost a fragment.
   */
  #admitSequence(event: TerminalEvent): TerminalEventAdmission {
    if (event.sequence === 0) {
      if (event.kind === "generationEpoch" && event.epoch !== this.#generationEpoch) {
        this.#lastSequence = 0;
      }
      return { kind: "local" };
    }
    if (event.kind === "snapshot" && event.authoritative) {
      this.#lastSequence = event.sequence;
      return { kind: "accepted" };
    }
    if (event.sequence <= this.#lastSequence) return { kind: "stale" };
    const expected = this.#lastSequence + 1;
    this.#lastSequence = event.sequence;
    if (event.sequence !== expected) this.#repairSequenceJump(expected, event.sequence);
    return { kind: "accepted" };
  }

  #repairSequenceJump(expected: number, received: number): void {
    const now = Date.now();
    if (now - this.#lastSequenceRepairAt < SEQUENCE_REPAIR_DEBOUNCE_MS) return;
    this.#lastSequenceRepairAt = now;
    this.measurements?.add("terminal.hub.sequenceJumps");
    // Journalled unconditionally: this path is meant to be unreachable now that
    // the native layer repairs breaks in place, and the journal is the only way
    // to learn that it fires in the field.
    recordIncident("link.eventGap", { expected, received });
    this.reseedSubscribedPanes("event sequence jumped (decode drop?)");
  }

  #requestConflictReseed(paneId: string, pane: PaneStreamState): void {
    if (pane.conflictReseedRequested) return;
    pane.conflictReseedRequested = true;
    this.#requestSeed(paneId, "stale or conflicting terminal visibility handoff");
  }

  #notifyHealthChange(paneId: string, before: PaneHealth): void {
    if (!this.#paneHealthListeners.has(paneId)) return;
    if (samePaneHealth(before, this.paneHealth(paneId))) return;
    this.#notifyHealth(paneId);
  }

  #notifyHealth(paneId: string): void {
    const listener = this.#paneHealthListeners.get(paneId);
    if (!listener) return;
    try {
      listener(this.paneHealth(paneId));
    } catch (error) {
      // Contained like every other observer here: a health report is a hint for
      // recovery, and it must never be able to break the delivery it rides on.
      this.#reportObserverFailure("terminal pane health observer", error);
    }
  }

  #requestSeed(paneId: string, reason: string): void {
    try {
      this.onSeedRequired?.(paneId, reason);
    } catch (error) {
      this.#reportObserverFailure("terminal seed-request observer", error);
    }
  }

  #reportObserverFailure(context: string, error: unknown): void {
    this.measurements?.add("terminal.hub.observerFailures");
    const message = `${context} failed after terminal event ownership transfer: ${String(error)}`;
    try {
      this.onObserverFailure?.(message);
    } catch {
      // This callback is the last-resort diagnostic/reconnect boundary. It is
      // intentionally contained as well: no observer may escape and create a
      // cumulative delivery hole after the hub owns a decoded frame.
      this.measurements?.add("terminal.hub.observerFailureHandlerFailures");
    }
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
