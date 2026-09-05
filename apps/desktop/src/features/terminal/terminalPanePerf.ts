import { perfProbeEnabled, recordPerfRecord } from "../../perf/probe";
import { perfProbeReady } from "../../perf/bootstrap";
import type { TerminalWriteObservation } from "./TerminalWriteScheduler";

const REPORT_INTERVAL_MS = 2_000;

/**
 * Per-pane rendering work, aggregated so a busy terminal cannot turn the perf
 * log itself into the bottleneck. It exists only for an opted-in perf launch.
 */
export class TerminalPanePerf {
  #startedAt = performance.now();
  #inputBytes = 0;
  #enqueueRecords = 0;
  #frameRequests = 0;
  #xtermWrites = 0;
  #xtermWriteBytes = 0;
  #xtermWriteMs = 0;
  #renderEvents = 0;
  #renderedRows = 0;
  #maxPendingBytes = 0;
  #maxQueueDepth = 0;

  constructor(readonly paneId: string) {}

  observe = (event: TerminalWriteObservation): void => {
    switch (event.kind) {
      case "enqueue":
        this.#inputBytes += event.bytes;
        this.#enqueueRecords += 1;
        this.#maxPendingBytes = Math.max(this.#maxPendingBytes, event.pendingBytes);
        this.#maxQueueDepth = Math.max(this.#maxQueueDepth, event.queueDepth);
        break;
      case "frameRequest":
        this.#frameRequests += 1;
        break;
      case "writeStarted":
        this.#xtermWrites += 1;
        this.#xtermWriteBytes += event.bytes;
        this.#maxPendingBytes = Math.max(this.#maxPendingBytes, event.pendingBytes);
        this.#maxQueueDepth = Math.max(this.#maxQueueDepth, event.queueDepth);
        break;
      case "writeSettled":
        this.#xtermWriteMs += event.ms;
        break;
    }
    this.#flush(false);
  };

  render(startRow: number, endRow: number): void {
    this.#renderEvents += 1;
    this.#renderedRows += Math.max(0, endRow - startRow + 1);
    this.#flush(false);
  }

  dispose(): void {
    this.#flush(true);
  }

  #flush(force: boolean): void {
    const now = performance.now();
    const intervalMs = now - this.#startedAt;
    if (!force && intervalMs < REPORT_INTERVAL_MS) return;
    if (this.#enqueueRecords > 0 || this.#renderEvents > 0 || this.#frameRequests > 0) {
      recordPerfRecord("perf.terminalPane", {
        paneId: this.paneId,
        intervalMs,
        inputBytes: this.#inputBytes,
        enqueueRecords: this.#enqueueRecords,
        frameRequests: this.#frameRequests,
        xtermWrites: this.#xtermWrites,
        xtermWriteBytes: this.#xtermWriteBytes,
        xtermWriteMs: this.#xtermWriteMs,
        renderEvents: this.#renderEvents,
        renderedRows: this.#renderedRows,
        maxPendingBytes: this.#maxPendingBytes,
        maxQueueDepth: this.#maxQueueDepth,
      });
    }
    this.#startedAt = now;
    this.#inputBytes = 0;
    this.#enqueueRecords = 0;
    this.#frameRequests = 0;
    this.#xtermWrites = 0;
    this.#xtermWriteBytes = 0;
    this.#xtermWriteMs = 0;
    this.#renderEvents = 0;
    this.#renderedRows = 0;
    this.#maxPendingBytes = 0;
    this.#maxQueueDepth = 0;
  }
}

export function createTerminalPanePerf(paneId: string | undefined): TerminalPanePerf | undefined {
  return paneId && perfProbeEnabled() ? new TerminalPanePerf(paneId) : undefined;
}

/**
 * Resolves the bootstrap race for renderers created while native perf opt-in is
 * still in flight. Callers should first try the synchronous factory above so
 * an already-enabled launch cannot miss its earliest writes.
 */
export async function terminalPanePerfWhenReady(
  paneId: string | undefined,
): Promise<TerminalPanePerf | undefined> {
  if (!paneId || !(await perfProbeReady())) return undefined;
  return new TerminalPanePerf(paneId);
}
