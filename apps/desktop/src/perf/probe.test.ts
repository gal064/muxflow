import { afterEach, describe, expect, it, vi } from "vitest";
import perfLogContract from "../../perf-log-contract.json";
import {
  enablePerfProbe,
  flushPerfProbe,
  measurePerfRequest,
  closePanePaintSpans,
  openPanePaintSpan,
  targetPanePaintSpan,
  perfCounterSnapshot,
  perfSummary,
  recordPerfCounter,
  recordPerfHighWater,
  recordPerfJsonBytes,
  recordPerfJsonBytesDeferred,
  recordPerfRecord,
  recordPerfSample,
  resetPerfProbe,
} from "./probe";

afterEach(() => resetPerfProbe());

describe("performance operation snapshots", () => {
  it("does not serialize payloads while the probe is disabled", () => {
    const value = { toJSON: () => { throw new Error("must remain inert"); } };
    expect(() => recordPerfJsonBytes("payload", value)).not.toThrow();
  });

  it("defers response serialization beyond the request's current task", async () => {
    vi.useFakeTimers();
    const serialize = vi.fn(() => ({ ok: true }));
    try {
      enablePerfProbe(async () => undefined);
      recordPerfJsonBytesDeferred("response.bytes", { toJSON: serialize });
      expect(serialize).not.toHaveBeenCalled();
      await vi.runOnlyPendingTimersAsync();
      expect(serialize).toHaveBeenCalledTimes(1);
      expect(perfCounterSnapshot()["response.bytes"]).toBeGreaterThan(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it("coalesces hot-path counters into one bounded flush record", async () => {
    const append = vi.fn(async (_lines: string[]) => undefined);
    enablePerfProbe(append);
    for (let index = 0; index < 10_000; index += 1) {
      recordPerfCounter("terminal.events");
      recordPerfHighWater("terminal.queueDepth", index);
    }

    await flushPerfProbe();

    expect(append).toHaveBeenCalledTimes(1);
    const lines = append.mock.calls[0][0];
    expect(lines).toHaveLength(1);
    expect(JSON.parse(lines[0])).toMatchObject({
      kind: "operations",
      counters: { "terminal.events": 10_000 },
      highWater: { "terminal.queueDepth": 9_999 },
    });
  });

  it("retries the exact batch after a transient append failure", async () => {
    const append = vi.fn()
      .mockRejectedValueOnce(new Error("temporary sink failure"))
      .mockResolvedValue(undefined);
    enablePerfProbe(append);
    recordPerfSample("terminal.event", 3);

    await flushPerfProbe();
    await flushPerfProbe();

    expect(append).toHaveBeenCalledTimes(2);
    expect(append.mock.calls[1][0]).toEqual(append.mock.calls[0][0]);
  });

  it("drains observations recorded while an append is in flight", async () => {
    let release: (() => void) | undefined;
    const firstAppend = new Promise<void>((resolve) => { release = resolve; });
    const append = vi.fn()
      .mockImplementationOnce(() => firstAppend)
      .mockResolvedValue(undefined);
    enablePerfProbe(append);
    recordPerfSample("first", 1);

    const flush = flushPerfProbe();
    await vi.waitFor(() => expect(append).toHaveBeenCalledTimes(1));
    recordPerfSample("second", 2);
    release?.();
    await flush;

    expect(append).toHaveBeenCalledTimes(2);
    expect(append.mock.calls[1][0].some((line: string) => JSON.parse(line).name === "second")).toBe(true);
  });

  it("chunks more than the native line limit without losing samples", async () => {
    const append = vi.fn(async (_lines: string[]) => undefined);
    enablePerfProbe(append);
    for (let index = 0; index < 5_000; index += 1) recordPerfSample("burst", index);

    await flushPerfProbe();

    expect(append.mock.calls.length).toBeGreaterThan(1);
    expect(append.mock.calls.every(([lines]) => lines.length <= perfLogContract.maxLinesPerAppend)).toBe(true);
    const samples = append.mock.calls.flatMap(([lines]) => lines).filter((line) => JSON.parse(line).name === "burst");
    expect(samples).toHaveLength(5_000);
  });

  it("byte-bounds a full-cardinality aggregate and retries it exactly", async () => {
    const append = vi.fn()
      .mockRejectedValueOnce(new Error("transient native failure"))
      .mockResolvedValue(undefined);
    enablePerfProbe(append);
    for (let index = 0; index < 700; index += 1) {
      recordPerfCounter(`fixture.counter.${index.toString().padStart(4, "0")}.${"λ".repeat(12)}`, index);
      recordPerfSample(`fixture.sample.${index.toString().padStart(4, "0")}`, index / 10);
    }

    await flushPerfProbe();
    await flushPerfProbe();

    expect(append.mock.calls[1][0]).toEqual(append.mock.calls[0][0]);
    const lines = append.mock.calls.flatMap(([batch]) => batch);
    expect(lines.every((line) => new TextEncoder().encode(line).byteLength <= perfLogContract.maxLineBytes)).toBe(true);
    expect(lines.filter((line) => JSON.parse(line).kind === "operations").length).toBeGreaterThan(1);
    expect(lines.filter((line) => JSON.parse(line).kind === "summary").length).toBeGreaterThan(1);
  });

  it("drops one individually oversized aggregate row without poisoning later flushes", async () => {
    const append = vi.fn(async (_lines: string[]) => undefined);
    enablePerfProbe(append);
    recordPerfCounter(`fixture.${"x".repeat(perfLogContract.maxLineBytes)}`);
    await flushPerfProbe();
    recordPerfCounter("fixture.afterOversize");
    await flushPerfProbe();

    const records = append.mock.calls.flatMap(([batch]) => batch).map((line) => JSON.parse(line));
    expect(records).toContainEqual(expect.objectContaining({
      kind: "sinkInvalid", reason: "aggregate row exceeds UTF-8 byte limit", droppedLines: 1,
    }));
    expect(records).toContainEqual(expect.objectContaining({
      kind: "operations", counters: expect.objectContaining({ "fixture.afterOversize": 1 }),
    }));
    expect(append.mock.calls.flatMap(([batch]) => batch)
      .every((line) => new TextEncoder().encode(line).byteLength <= perfLogContract.maxLineBytes)).toBe(true);
  });

  it("stamps correlation fields onto the emitted sample record only", async () => {
    const append = vi.fn(async (_lines: string[]) => undefined);
    enablePerfProbe(append);
    recordPerfSample("file.open.segment.queueWait", 12.5, { operationId: "open-1" });

    await flushPerfProbe();

    const records = append.mock.calls.flatMap(([lines]) => lines).map((line) => JSON.parse(line));
    expect(records).toContainEqual(expect.objectContaining({
      name: "file.open.segment.queueWait", ms: 12.5, operationId: "open-1",
    }));
    // The summary aggregates by name alone; correlation never forks a series.
    expect(perfSummary().map(({ name }) => name)).toEqual(["file.open.segment.queueWait"]);
  });

  it("keeps global and domain request outcomes on one invariant", async () => {
    enablePerfProbe(async () => undefined);
    await expect(measurePerfRequest(
      "request.ok", "fixture", { a: 1 }, async () => "ok",
    )).resolves.toBe("ok");
    await expect(measurePerfRequest("request.failed", "fixture", {}, async () => {
      throw new Error("rejected response");
    })).rejects.toThrow("rejected response");
    await expect(measurePerfRequest("request.cancelled", "fixture", {}, async () => {
      throw new DOMException("cancelled", "AbortError");
    })).rejects.toMatchObject({ name: "AbortError" });

    const counters = perfCounterSnapshot();
    expect(counters["desktop.hostRequestAttempts"]).toBe(3);
    expect(counters["desktop.hostRequestSuccesses"]).toBe(1);
    expect(counters["desktop.hostRequestFailures"]).toBe(1);
    expect(counters["desktop.hostRequestCancellations"]).toBe(1);
    expect(counters["desktop.hostRequestBytes"]).toBe(11);
    expect(counters["fixture.hostRequestAttempts"]).toBe(3);
    expect(counters["fixture.hostRequestSuccesses"]
      + counters["fixture.hostRequestFailures"]
      + counters["fixture.hostRequestCancellations"]).toBe(3);
    expect(counters["fixture.hostRequestBytes"]).toBe(11);
  });

  it("closes concurrent workflow spans only for their authoritative pane target", () => {
    enablePerfProbe(async () => undefined);
    const tab = openPanePaintSpan("create.tab", "client-a");
    const split = openPanePaintSpan("pane.split", "client-a");
    targetPanePaintSpan(tab, "%1");
    targetPanePaintSpan(split, "%2");

    closePanePaintSpans("client-a", "%2");
    expect(perfSummary().map(({ name }) => name)).toEqual(["pane.split"]);
    closePanePaintSpans("client-b", "%1");
    expect(perfSummary().map(({ name }) => name)).toEqual(["pane.split"]);
    closePanePaintSpans("client-a", "%1");
    expect(perfSummary().map(({ name }) => name)).toEqual(["create.tab", "pane.split"]);

    const lateAck = openPanePaintSpan("window.switch", "client-a");
    closePanePaintSpans("client-a", "%3");
    targetPanePaintSpan(lateAck, "%3");
    expect(perfSummary().map(({ name }) => name)).toEqual(["create.tab", "pane.split", "window.switch"]);
  });

  it("bounds workflow spans whose authoritative pane never paints", () => {
    enablePerfProbe(async () => undefined);
    for (let index = 0; index < 257; index += 1) {
      openPanePaintSpan("create.tab", `client-${index}`);
    }

    expect(perfCounterSnapshot()).toMatchObject({ "workflow.panePaintSpanCapacityDrops": 1 });
  });

  it("writes a structured record as one line and never lets its fields frame it", async () => {
    const appended: string[] = [];
    enablePerfProbe(async (lines) => { appended.push(...lines); });

    recordPerfRecord("perf.timeline", {
      action: "selectSession",
      kind: "must not overwrite the record's own label",
      d1: 1_700_000_000_000,
      framesAheadByKind: { terminalSeed: 4 },
      absent: undefined,
    });
    await flushPerfProbe();

    const record = JSON.parse(appended[0]) as Record<string, unknown>;
    expect(record).toMatchObject({
      kind: "perf.timeline",
      action: "selectSession",
      d1: 1_700_000_000_000,
      framesAheadByKind: { terminalSeed: 4 },
    });
    expect(record).not.toHaveProperty("absent");
    expect(appended[0]).not.toContain("\n");
  });

  it("records nothing structured while the probe is disabled", () => {
    recordPerfRecord("perf.echo", { lagMs: 12 });
    expect(perfSummary()).toEqual([]);
  });

  it("keeps an unserializable measurement payload from affecting the request", async () => {
    enablePerfProbe(async () => undefined);

    await expect(measurePerfRequest(
      "request.bigint", "fixture", { value: 1n }, async () => "ok",
    )).resolves.toBe("ok");

    expect(perfCounterSnapshot()).toMatchObject({
      "desktop.hostRequestAttempts": 1,
      "desktop.hostRequestSuccesses": 1,
      "desktop.hostRequestByteMeasurementFailures": 1,
      "fixture.hostRequestByteMeasurementFailures": 1,
    });
  });
});
