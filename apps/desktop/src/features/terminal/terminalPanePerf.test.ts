import { afterEach, describe, expect, it, vi } from "vitest";
import { enablePerfProbe, flushPerfProbe, resetPerfProbe } from "../../perf/probe";
import { TerminalPanePerf } from "./terminalPanePerf";

afterEach(() => {
  vi.useRealTimers();
  resetPerfProbe();
});

describe("per-pane terminal performance accounting", () => {
  it("emits one bounded aggregate rather than one record per write", async () => {
    const lines: string[] = [];
    enablePerfProbe(async (batch) => { lines.push(...batch); });
    const perf = new TerminalPanePerf("%7", 41);

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
      connectionEpoch: 41,
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

  it("closes the old interval before adopting a reconnect epoch", async () => {
    const lines: string[] = [];
    enablePerfProbe(async (batch) => { lines.push(...batch); });
    const perf = new TerminalPanePerf("%7", 41);

    perf.observe({ kind: "enqueue", bytes: 1, pendingBytes: 1, queueDepth: 1 });
    perf.setConnectionEpoch(42);
    perf.observe({ kind: "enqueue", bytes: 2, pendingBytes: 2, queueDepth: 1 });
    perf.dispose();
    await flushPerfProbe();

    const records = lines.map((line) => JSON.parse(line) as Record<string, unknown>)
      .filter((candidate) => candidate.kind === "perf.terminalPane");
    expect(records).toHaveLength(2);
    expect(records[0]).toMatchObject({ connectionEpoch: 41, inputBytes: 1 });
    expect(records[1]).toMatchObject({ connectionEpoch: 42, inputBytes: 2 });
  });

  it("settles an in-flight write into the epoch where it began", async () => {
    const lines: string[] = [];
    enablePerfProbe(async (batch) => { lines.push(...batch); });
    const perf = new TerminalPanePerf("%7", 41);
    const oldObserver = perf.observe;

    oldObserver({ kind: "writeStarted", bytes: 12, records: 1, pendingBytes: 12, queueDepth: 0 });
    perf.setConnectionEpoch(42);
    const newObserver = perf.observe;
    newObserver({ kind: "enqueue", bytes: 2, pendingBytes: 2, queueDepth: 1 });
    oldObserver({ kind: "writeSettled", bytes: 12, ms: 3.5, succeeded: true });
    perf.dispose();
    await flushPerfProbe();

    const records = lines.map((line) => JSON.parse(line) as Record<string, unknown>)
      .filter((candidate) => candidate.kind === "perf.terminalPane");
    expect(records).toHaveLength(2);
    expect(records[0]).toMatchObject({
      connectionEpoch: 41,
      xtermWrites: 1,
      xtermWriteMs: 3.5,
    });
    expect(records[1]).toMatchObject({ connectionEpoch: 42, inputBytes: 2 });
  });

  it("finalizes a disposed in-flight write as incomplete", async () => {
    const lines: string[] = [];
    enablePerfProbe(async (batch) => { lines.push(...batch); });
    const perf = new TerminalPanePerf("%7", 41);
    const observer = perf.observe;

    observer({ kind: "writeStarted", bytes: 12, records: 1, pendingBytes: 12, queueDepth: 0 });
    perf.dispose();
    observer({ kind: "writeSettled", bytes: 12, ms: 3.5, succeeded: true });
    await flushPerfProbe();

    const records = lines.map((line) => JSON.parse(line) as Record<string, unknown>)
      .filter((candidate) => candidate.kind === "perf.terminalPane");
    expect(records).toHaveLength(1);
    expect(records[0]).toMatchObject({
      connectionEpoch: 41,
      xtermWrites: 1,
      xtermWriteMs: 0,
      incompleteWrites: 1,
    });
  });

  it("flushes an active interval after two seconds without another event", async () => {
    vi.useFakeTimers();
    const lines: string[] = [];
    enablePerfProbe(async (batch) => { lines.push(...batch); });
    const perf = new TerminalPanePerf("%7", 41);

    perf.observe({ kind: "enqueue", bytes: 5, pendingBytes: 5, queueDepth: 1 });
    await vi.advanceTimersByTimeAsync(2_000);
    await flushPerfProbe();

    const record = lines.map((line) => JSON.parse(line) as Record<string, unknown>)
      .find((candidate) => candidate.kind === "perf.terminalPane");
    expect(record).toMatchObject({ connectionEpoch: 41, inputBytes: 5 });
    perf.dispose();
  });
});
