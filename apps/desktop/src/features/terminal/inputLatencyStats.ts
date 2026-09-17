/**
 * Where the milliseconds of a keystroke go, aggregated for the journal.
 *
 * `echoLagProbe` answers "did this echo take too long"; it cannot answer
 * *where* the time went. A felt keystroke crosses four measurable segments —
 * the send invoke, the native input queue, the network plus daemon plus tmux,
 * and the paint — and only the first, second and fourth can be timed directly.
 * Each one gets a log-bucketed histogram here, and one journal record per
 * minute of active typing carries all of them; the residual (endToEnd minus
 * send, rustQueue and paint) is the remote half, computed when the journal is
 * read rather than in the app.
 *
 * Journal-only, and cheap enough to be always on: a sample is one comparison
 * loop over twenty bucket bounds and one counter increment, an idle app records
 * nothing and does not even ask the native side for its half.
 */

/**
 * Upper bounds, in milliseconds, of every latency bucket.
 *
 * SHARED WITH RUST: `INPUT_LATENCY_BUCKET_BOUNDS_MS` in
 * `src-tauri/src/connection/dispatch.rs` is the same list in the same order, so
 * the native queue histogram and these read as one distribution. Changing
 * either side without the other silently mislabels the native buckets.
 */
export const INPUT_LATENCY_BUCKET_BOUNDS_MS: readonly number[] = [
  1, 2, 4, 8, 16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 2048, 4096, Infinity,
];

/** One aggregation window of one segment. All figures are milliseconds. */
export interface LatencySnapshot {
  count: number;
  p50: number;
  p90: number;
  p99: number;
  max: number;
}

export interface LatencyHistogram {
  record(ms: number): void;
  /** Reads the window and starts a new one; an empty window reads as zeroes. */
  snapshotAndReset(): LatencySnapshot;
}

/** The first bucket whose upper bound the value does not exceed. */
export function inputLatencyBucketIndex(ms: number): number {
  for (let index = 0; index < INPUT_LATENCY_BUCKET_BOUNDS_MS.length; index += 1) {
    if (ms <= INPUT_LATENCY_BUCKET_BOUNDS_MS[index]) return index;
  }
  return INPUT_LATENCY_BUCKET_BOUNDS_MS.length - 1;
}

/**
 * Turns bucket counts into the reported shape.
 *
 * The percentiles are approximations by construction: a bucketed histogram
 * knows how many samples landed in a range, not where in it, so a percentile is
 * reported as the *upper bound* of the bucket the nearest-rank sample falls in
 * — p50 of a window whose median keystroke took 5 ms reads as 8. Only the top
 * bucket is unbounded, and there the true observed maximum is reported instead
 * of `Infinity`. `max` is always exact: it is tracked alongside the buckets, on
 * both sides of the IPC, precisely because no bucket can recover it.
 *
 * Shared with the native queue histogram, which arrives as raw bucket counts.
 */
export function snapshotFromBuckets(counts: readonly number[], max: number): LatencySnapshot {
  let total = 0;
  for (const count of counts) total += count;
  if (total === 0) return { count: 0, p50: 0, p90: 0, p99: 0, max: 0 };
  const percentile = (fraction: number): number => {
    const rank = Math.max(1, Math.ceil(fraction * total));
    let cumulative = 0;
    for (let index = 0; index < counts.length; index += 1) {
      cumulative += counts[index];
      if (cumulative < rank) continue;
      const bound = INPUT_LATENCY_BUCKET_BOUNDS_MS[index];
      return Number.isFinite(bound) ? bound : max;
    }
    return max;
  };
  return { count: total, p50: percentile(0.5), p90: percentile(0.9), p99: percentile(0.99), max };
}

export function createLatencyHistogram(): LatencyHistogram {
  const counts = new Array<number>(INPUT_LATENCY_BUCKET_BOUNDS_MS.length).fill(0);
  let max = 0;
  return {
    record(ms) {
      if (!Number.isFinite(ms) || ms < 0) return;
      counts[inputLatencyBucketIndex(ms)] += 1;
      if (ms > max) max = ms;
    },
    snapshotAndReset() {
      const snapshot = snapshotFromBuckets(counts, max);
      counts.fill(0);
      max = 0;
      return snapshot;
    },
  };
}

/** The native input queue's window, as `input_latency_stats` returns it. */
export interface RustInputLatencyHistogram {
  bucketCounts: number[];
  maxMs: number;
}

/** The segments the frontend can time by itself. */
export type InputLatencySegment = "endToEnd" | "send" | "paint";

export interface InputLatencyReporterOptions {
  recordIncident: (kind: string, detail?: Record<string, unknown>) => void;
  /** Reads *and drains* the native queue histogram; `null` when unavailable. */
  fetchRustHistogram: () => Promise<RustInputLatencyHistogram | null>;
  flushIntervalMs?: number;
}

export interface InputLatencyReporter {
  sample(segment: InputLatencySegment, ms: number): void;
  dispose(): void;
}

/** One aggregated record per minute of typing, and nothing while idle. */
export const INPUT_LATENCY_FLUSH_INTERVAL_MS = 60_000;

export function createInputLatencyReporter({
  recordIncident,
  fetchRustHistogram,
  flushIntervalMs = INPUT_LATENCY_FLUSH_INTERVAL_MS,
}: InputLatencyReporterOptions): InputLatencyReporter {
  const histograms: Record<InputLatencySegment, LatencyHistogram> = {
    endToEnd: createLatencyHistogram(),
    send: createLatencyHistogram(),
    paint: createLatencyHistogram(),
  };

  const emit = (
    endToEnd: LatencySnapshot,
    send: LatencySnapshot,
    paint: LatencySnapshot,
    rust: RustInputLatencyHistogram | null,
  ): void => {
    try {
      // A native half that arrived malformed is dropped rather than reported as
      // buckets that do not mean what their bounds say.
      const usable = rust
        && Array.isArray(rust.bucketCounts)
        && rust.bucketCounts.length === INPUT_LATENCY_BUCKET_BOUNDS_MS.length;
      recordIncident("input.latency", {
        endToEnd,
        send,
        paint,
        rustQueue: usable ? snapshotFromBuckets(rust.bucketCounts, rust.maxMs) : undefined,
      });
    } catch {
      // A diagnostic that throws into the app is worse than a missing record.
    }
  };

  const flush = (): void => {
    try {
      // The end-to-end histogram is the one the whole record exists to explain:
      // with nothing in it there was no typing this window, and an idle app
      // must stay silent — and must not pay for the native round trip either.
      const endToEnd = histograms.endToEnd.snapshotAndReset();
      if (endToEnd.count === 0) return;
      const send = histograms.send.snapshotAndReset();
      const paint = histograms.paint.snapshotAndReset();
      void Promise.resolve(fetchRustHistogram()).then(
        (rust) => emit(endToEnd, send, paint, rust),
        () => emit(endToEnd, send, paint, null),
      );
    } catch {
      // Same contract as `emit`: never throw out of the timer.
    }
  };

  const timer = setInterval(flush, flushIntervalMs);

  return {
    sample(segment, ms) {
      try {
        histograms[segment]?.record(ms);
      } catch {
        // A bad segment name loses one sample and nothing else.
      }
    },
    dispose() {
      clearInterval(timer);
    },
  };
}
