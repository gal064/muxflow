import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  createInputLatencyReporter,
  createLatencyHistogram,
  INPUT_LATENCY_BUCKET_BOUNDS_MS,
  inputLatencyBucketIndex,
  snapshotFromBuckets,
  type RustInputLatencyHistogram,
} from "./inputLatencyStats";

describe("inputLatencyBucketIndex", () => {
  it("places a value in the first bucket whose bound it does not exceed", () => {
    expect(inputLatencyBucketIndex(0)).toBe(0);
    expect(inputLatencyBucketIndex(1)).toBe(0);
    expect(inputLatencyBucketIndex(1.5)).toBe(1);
    expect(inputLatencyBucketIndex(2)).toBe(1);
    expect(inputLatencyBucketIndex(2.1)).toBe(2);
    expect(inputLatencyBucketIndex(4096)).toBe(INPUT_LATENCY_BUCKET_BOUNDS_MS.length - 2);
    expect(inputLatencyBucketIndex(4097)).toBe(INPUT_LATENCY_BUCKET_BOUNDS_MS.length - 1);
    expect(inputLatencyBucketIndex(1e9)).toBe(INPUT_LATENCY_BUCKET_BOUNDS_MS.length - 1);
  });
});

describe("createLatencyHistogram", () => {
  it("reports an empty window as zeroes", () => {
    expect(createLatencyHistogram().snapshotAndReset()).toEqual({ count: 0, p50: 0, p90: 0, p99: 0, max: 0 });
  });

  it("reports each percentile as the upper bound of its bucket, and the true max", () => {
    const histogram = createLatencyHistogram();
    // 90 samples in the 8 ms bucket, 9 in the 64 ms bucket, one at 300 ms.
    for (let index = 0; index < 90; index += 1) histogram.record(5);
    for (let index = 0; index < 9; index += 1) histogram.record(50);
    histogram.record(300);
    expect(histogram.snapshotAndReset()).toEqual({ count: 100, p50: 8, p90: 8, p99: 64, max: 300 });
  });

  it("reports the observed max where the percentile lands in the unbounded bucket", () => {
    const histogram = createLatencyHistogram();
    histogram.record(9_000);
    expect(histogram.snapshotAndReset()).toEqual({ count: 1, p50: 9_000, p90: 9_000, p99: 9_000, max: 9_000 });
  });

  it("ignores values that are not a duration", () => {
    const histogram = createLatencyHistogram();
    histogram.record(Number.NaN);
    histogram.record(Number.POSITIVE_INFINITY);
    histogram.record(-1);
    expect(histogram.snapshotAndReset().count).toBe(0);
  });

  it("starts a new window after every snapshot", () => {
    const histogram = createLatencyHistogram();
    histogram.record(500);
    expect(histogram.snapshotAndReset()).toMatchObject({ count: 1, max: 500 });
    expect(histogram.snapshotAndReset()).toEqual({ count: 0, p50: 0, p90: 0, p99: 0, max: 0 });
  });
});

describe("snapshotFromBuckets", () => {
  it("reads native bucket counts as one distribution with the frontend's bounds", () => {
    const counts = new Array<number>(INPUT_LATENCY_BUCKET_BOUNDS_MS.length).fill(0);
    counts[0] = 3;
    counts[4] = 1;
    expect(snapshotFromBuckets(counts, 12)).toEqual({ count: 4, p50: 1, p90: 16, p99: 16, max: 12 });
  });
});

function reporterHarness(rust: RustInputLatencyHistogram | null) {
  const records: Array<{ kind: string; detail?: Record<string, unknown> }> = [];
  const fetchRustHistogram = vi.fn(async () => rust);
  const reporter = createInputLatencyReporter({
    recordIncident: (kind, detail) => records.push({ kind, detail }),
    fetchRustHistogram,
    flushIntervalMs: 1_000,
  });
  return { fetchRustHistogram, records, reporter };
}

describe("createInputLatencyReporter", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("emits one record carrying every segment", async () => {
    const nativeCounts = new Array<number>(INPUT_LATENCY_BUCKET_BOUNDS_MS.length).fill(0);
    nativeCounts[1] = 2;
    const { fetchRustHistogram, records, reporter } = reporterHarness({ bucketCounts: nativeCounts, maxMs: 2 });
    reporter.sample("endToEnd", 300);
    reporter.sample("send", 3);
    reporter.sample("paint", 6);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(records).toHaveLength(1);
    expect(fetchRustHistogram).toHaveBeenCalledTimes(1);
    expect(records[0].kind).toBe("input.latency");
    expect(records[0].detail).toEqual({
      endToEnd: { count: 1, p50: 384, p90: 384, p99: 384, max: 300 },
      send: { count: 1, p50: 4, p90: 4, p99: 4, max: 3 },
      paint: { count: 1, p50: 8, p90: 8, p99: 8, max: 6 },
      rustQueue: { count: 2, p50: 2, p90: 2, p99: 2, max: 2 },
    });
    reporter.dispose();
  });

  it("stays silent, and asks the native side for nothing, with no end-to-end sample", async () => {
    const { fetchRustHistogram, records, reporter } = reporterHarness(null);
    // A paint sample alone is not a keystroke: output arrives in panes nobody is
    // typing into, and an idle app must cost nothing.
    reporter.sample("paint", 9);
    await vi.advanceTimersByTimeAsync(5_000);
    expect(records).toEqual([]);
    expect(fetchRustHistogram).not.toHaveBeenCalled();
    reporter.dispose();
  });

  it("records the frontend half when the native half is unavailable", async () => {
    const { records, reporter } = reporterHarness(null);
    reporter.sample("endToEnd", 20);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(records).toHaveLength(1);
    expect(records[0].detail).toMatchObject({ rustQueue: undefined });
    reporter.dispose();
  });

  it("survives a native read that rejects", async () => {
    const records: Array<{ kind: string; detail?: Record<string, unknown> }> = [];
    const reporter = createInputLatencyReporter({
      recordIncident: (kind, detail) => records.push({ kind, detail }),
      fetchRustHistogram: () => Promise.reject(new Error("no client")),
      flushIntervalMs: 1_000,
    });
    reporter.sample("endToEnd", 20);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(records).toHaveLength(1);
    reporter.dispose();
  });

  it("starts a fresh window after each flush", async () => {
    const { records, reporter } = reporterHarness(null);
    reporter.sample("endToEnd", 20);
    reporter.sample("send", 20);
    await vi.advanceTimersByTimeAsync(1_000);
    reporter.sample("endToEnd", 1_000);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(records).toHaveLength(2);
    expect(records[1].detail).toMatchObject({
      endToEnd: { count: 1, max: 1_000 },
      send: { count: 0, max: 0 },
    });
    reporter.dispose();
  });

  it("stops flushing once disposed", async () => {
    const { records, reporter } = reporterHarness(null);
    reporter.sample("endToEnd", 20);
    reporter.dispose();
    await vi.advanceTimersByTimeAsync(10_000);
    expect(records).toEqual([]);
  });
});
