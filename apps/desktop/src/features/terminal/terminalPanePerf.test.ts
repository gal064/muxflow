import { afterEach, describe, expect, it } from "vitest";
import { enablePerfProbe, flushPerfProbe, resetPerfProbe } from "../../perf/probe";
import { TerminalPanePerf } from "./terminalPanePerf";

afterEach(() => resetPerfProbe());

describe("per-pane terminal performance accounting", () => {
  it("emits one bounded aggregate rather than one record per write", async () => {
    const lines: string[] = [];
    enablePerfProbe(async (batch) => { lines.push(...batch); });
    const perf = new TerminalPanePerf("%7");

    perf.observe({ kind: "enqueue", bytes: 12, pendingBytes: 12, queueDepth: 1 });
    perf.observe({ kind: "frameRequest" });
    perf.observe({ kind: "writeStarted", bytes: 12, records: 1, pendingBytes: 12, queueDepth: 0 });
    perf.observe({ kind: "writeSettled", bytes: 12, ms: 1.25, succeeded: true });
    perf.render(2, 5);
    perf.dispose();
    await flushPerfProbe();

    const record = lines.map((line) => JSON.parse(line) as Record<string, unknown>)
      .find((candidate) => candidate.kind === "perf.terminalPane");
    expect(record).toMatchObject({
      paneId: "%7",
      inputBytes: 12,
      enqueueRecords: 1,
      frameRequests: 1,
      xtermWrites: 1,
      xtermWriteBytes: 12,
      xtermWriteMs: 1.25,
      renderEvents: 1,
      renderedRows: 4,
      maxPendingBytes: 12,
      maxQueueDepth: 1,
    });
  });
});
