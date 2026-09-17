import { perfProbeEnabled, recordPerfRecord } from "../../perf/probe";
import { perfProbeReady } from "../../perf/bootstrap";
import type { TerminalWriteObservation } from "./TerminalWriteScheduler";

const REPORT_INTERVAL_MS = 2_000;

/**
 * Per-pane rendering work, aggregated so a busy terminal cannot turn the perf
 * log itself into the bottleneck. It exists only for an opted-in perf launch.
 */
export class TerminalPanePerf {
  #interval: PanePerfInterval;
  readonly #retired = new Set<PanePerfInterval>();
  #flushTimer: ReturnType<typeof setTimeout> | undefined;

  constructor(readonly paneId: string, connectionEpoch?: number) {
    this.#interval = createInterval(connectionEpoch);
  }

  setConnectionEpoch(connectionEpoch: number | undefined): void {
    if (connectionEpoch === this.#interval.connectionEpoch) return;
    this.#clearFlushTimer();
    const previous = this.#interval;
    previous.closing = true;
    this.#retired.add(previous);
    this.#interval = createInterval(connectionEpoch);
    this.#flush(previous, true);
  }

  /**
   * The scheduler captures one observer for an in-flight xterm write. Returning
   * a closure bound to this interval keeps a late settlement on the epoch in
   * which its write began, even after reconnect installs a new observer.
   */
  get observe(): (event: TerminalWriteObservation) => void {
    const interval = this.#interval;
    return (event) => this.#observe(interval, event);
  }

  #observe(interval: PanePerfInterval, event: TerminalWriteObservation): void {
    if (interval.finalized) return;
    switch (event.kind) {
      case "enqueue":
        interval.inputBytes += event.bytes;
        interval.enqueueRecords += 1;
        interval.maxPendingBytes = Math.max(interval.maxPendingBytes, event.pendingBytes);
        interval.maxQueueDepth = Math.max(interval.maxQueueDepth, event.queueDepth);
        break;
      case "frameRequest":
        interval.frameRequests += 1;
        break;
      case "writeStarted":
        interval.xtermWrites += 1;
        interval.pendingWrites += 1;
        interval.xtermWriteBytes += event.bytes;
        interval.maxPendingBytes = Math.max(interval.maxPendingBytes, event.pendingBytes);
        interval.maxQueueDepth = Math.max(interval.maxQueueDepth, event.queueDepth);
        break;
      case "writeSettled":
        interval.pendingWrites = Math.max(0, interval.pendingWrites - 1);
        interval.xtermWriteMs += event.ms;
        break;
    }
    this.#flush(interval, interval.closing);
    this.#armFlush(interval);
  }

  render(startRow: number, endRow: number): void {
    this.#interval.renderEvents += 1;
    this.#interval.renderedRows += Math.max(0, endRow - startRow + 1);
    this.#flush(this.#interval, false);
    this.#armFlush(this.#interval);
  }

  dispose(): void {
    this.#clearFlushTimer();
    this.#interval.closing = true;
    this.#retired.add(this.#interval);
    for (const interval of [...this.#retired]) {
      interval.incompleteWrites += interval.pendingWrites;
      interval.pendingWrites = 0;
      interval.finalized = true;
      this.#flush(interval, true);
    }
  }

  #flush(interval: PanePerfInterval, force: boolean): void {
    const now = performance.now();
    const intervalMs = now - interval.startedAt;
    if (interval.pendingWrites > 0 || (!force && intervalMs < REPORT_INTERVAL_MS)) return;
    if (interval.connectionEpoch !== undefined
      && (interval.enqueueRecords > 0
        || interval.renderEvents > 0
        || interval.frameRequests > 0
        || interval.xtermWrites > 0)) {
      recordPerfRecord("perf.terminalPane", {
        paneId: this.paneId,
        connectionEpoch: interval.connectionEpoch,
        intervalMs,
        inputBytes: interval.inputBytes,
        enqueueRecords: interval.enqueueRecords,
        frameRequests: interval.frameRequests,
        xtermWrites: interval.xtermWrites,
        xtermWriteBytes: interval.xtermWriteBytes,
        xtermWriteMs: interval.xtermWriteMs,
        incompleteWrites: interval.incompleteWrites,
        renderEvents: interval.renderEvents,
        renderedRows: interval.renderedRows,
        maxPendingBytes: interval.maxPendingBytes,
        maxQueueDepth: interval.maxQueueDepth,
      });
    }
    if (interval.closing) {
      this.#retired.delete(interval);
      return;
    }
    if (interval === this.#interval) this.#clearFlushTimer();
    resetInterval(interval, now);
  }

  #armFlush(interval: PanePerfInterval): void {
    if (interval !== this.#interval || interval.closing || this.#flushTimer !== undefined) return;
    const elapsed = performance.now() - interval.startedAt;
    if (interval.pendingWrites > 0 && elapsed >= REPORT_INTERVAL_MS) return;
    this.#flushTimer = setTimeout(() => {
      this.#flushTimer = undefined;
      if (interval !== this.#interval || interval.closing) return;
      this.#flush(interval, true);
    }, Math.max(0, REPORT_INTERVAL_MS - elapsed));
  }

  #clearFlushTimer(): void {
    if (this.#flushTimer === undefined) return;
    clearTimeout(this.#flushTimer);
    this.#flushTimer = undefined;
  }
}

interface PanePerfInterval {
  connectionEpoch: number | undefined;
  startedAt: number;
  inputBytes: number;
  enqueueRecords: number;
  frameRequests: number;
  xtermWrites: number;
  pendingWrites: number;
  incompleteWrites: number;
  xtermWriteBytes: number;
  xtermWriteMs: number;
  renderEvents: number;
  renderedRows: number;
  maxPendingBytes: number;
  maxQueueDepth: number;
  closing: boolean;
  finalized: boolean;
}

function createInterval(connectionEpoch: number | undefined): PanePerfInterval {
  return {
    connectionEpoch,
    startedAt: performance.now(),
    inputBytes: 0,
    enqueueRecords: 0,
    frameRequests: 0,
    xtermWrites: 0,
    pendingWrites: 0,
    incompleteWrites: 0,
    xtermWriteBytes: 0,
    xtermWriteMs: 0,
    renderEvents: 0,
    renderedRows: 0,
    maxPendingBytes: 0,
    maxQueueDepth: 0,
    closing: false,
    finalized: false,
  };
}

function resetInterval(interval: PanePerfInterval, now: number): void {
  const connectionEpoch = interval.connectionEpoch;
  Object.assign(interval, createInterval(connectionEpoch), { startedAt: now });
}

export function createTerminalPanePerf(
  paneId: string | undefined,
  connectionEpoch?: number,
): TerminalPanePerf | undefined {
  return paneId && perfProbeEnabled() ? new TerminalPanePerf(paneId, connectionEpoch) : undefined;
}

/**
 * Resolves the bootstrap race for renderers created while native perf opt-in is
 * still in flight. Callers should first try the synchronous factory above so
 * an already-enabled launch cannot miss its earliest writes.
 */
export async function terminalPanePerfWhenReady(
  paneId: string | undefined,
  connectionEpoch: () => number | undefined,
): Promise<TerminalPanePerf | undefined> {
  if (!paneId || !(await perfProbeReady())) return undefined;
  return new TerminalPanePerf(paneId, connectionEpoch());
}
