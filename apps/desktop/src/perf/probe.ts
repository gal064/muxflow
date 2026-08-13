/**
 * Phase 12 in-app latency instrumentation.
 *
 * Every Phase 12 budget has to be closed with a machine-measured number, and
 * several of them (keystroke to painted glyph, action to interactive pane, tab
 * switch, explorer expand) only exist inside the renderer. This module records
 * those spans with `performance.now()` and appends them to a file the QA
 * harness reads afterwards.
 *
 * It is inert unless the desktop process was started with `ADE_PERF_LOG` set:
 * the host command refuses to write without it, and the frontend never enables
 * itself on its own. Nothing here changes what the user sees.
 */

export interface PerfSummaryRow {
  name: string;
  n: number;
  meanMs: number;
  p50Ms: number;
  p95Ms: number;
  maxMs: number;
}

/** Per-metric sample cap. Old samples are dropped, never the newest ones. */
const MAX_SAMPLES = 1_024;
/** Flush cadence for the append-only log. */
const FLUSH_INTERVAL_MS = 2_000;

type Appender = (lines: string[]) => Promise<void>;

const samples = new Map<string, number[]>();
const openSpans = new Map<string, number>();
let pending: string[] = [];
let enabled = false;
let appender: Appender | undefined;
let flushTimer: ReturnType<typeof setTimeout> | undefined;

function now(): number {
  return typeof performance === "undefined" ? Date.now() : performance.now();
}

/**
 * Turns instrumentation on and installs the sink that persists samples. The
 * caller is responsible for having proven that the process opted in.
 */
export function enablePerfProbe(sink: Appender): void {
  enabled = true;
  appender = sink;
}

export function perfProbeEnabled(): boolean {
  return enabled;
}

export function resetPerfProbe(): void {
  enabled = false;
  appender = undefined;
  samples.clear();
  openSpans.clear();
  pending = [];
  if (flushTimer !== undefined) clearTimeout(flushTimer);
  flushTimer = undefined;
}

export function recordPerfSample(name: string, milliseconds: number): void {
  if (!enabled || !Number.isFinite(milliseconds) || milliseconds < 0) return;
  const bucket = samples.get(name) ?? [];
  bucket.push(milliseconds);
  if (bucket.length > MAX_SAMPLES) bucket.shift();
  samples.set(name, bucket);
  pending.push(JSON.stringify({ t: Date.now(), name, ms: Math.round(milliseconds * 1000) / 1000 }));
  scheduleFlush();
}

/** Times a span that both begins and ends at a known call site. */
export function startPerfSpan(name: string): () => void {
  if (!enabled) return () => undefined;
  const started = now();
  let closed = false;
  return () => {
    if (closed) return;
    closed = true;
    recordPerfSample(name, now() - started);
  };
}

/**
 * Opens a span whose end is observed somewhere else entirely — a create action
 * ends when a pane paints, several components away from the click that started
 * it. Re-opening an already open span keeps the original start, so a burst of
 * clicks measures the user's whole wait rather than only the last one.
 */
export function openPerfSpan(name: string): void {
  if (!enabled || openSpans.has(name)) return;
  openSpans.set(name, now());
}

/** Closes an open cross-component span. Closing an unopened span is a no-op. */
export function closePerfSpan(name: string): void {
  if (!enabled) return;
  const started = openSpans.get(name);
  if (started === undefined) return;
  openSpans.delete(name);
  recordPerfSample(name, now() - started);
}

export function abandonPerfSpan(name: string): void {
  openSpans.delete(name);
}

/**
 * User interactions whose completion the user perceives as "the terminal is
 * showing me something": each ends when a pane finishes painting its first
 * content, wherever in the tree that happens.
 */
export const PANE_PAINT_SPANS = ["create.tab", "create.workspace", "pane.split", "window.switch"] as const;

export function closePanePaintSpans(): void {
  for (const name of PANE_PAINT_SPANS) closePerfSpan(name);
}

/** Times an awaited call without changing its result or its rejection. */
export async function measurePerf<T>(name: string, work: () => Promise<T>): Promise<T> {
  if (!enabled) return work();
  const close = startPerfSpan(name);
  try {
    return await work();
  } finally {
    close();
  }
}

function percentile(sorted: readonly number[], fraction: number): number {
  if (sorted.length === 0) return 0;
  return sorted[Math.round((sorted.length - 1) * fraction)];
}

export function perfSummary(): PerfSummaryRow[] {
  return [...samples.entries()]
    .map(([name, values]) => {
      const sorted = [...values].sort((left, right) => left - right);
      const total = sorted.reduce((sum, value) => sum + value, 0);
      const round = (value: number) => Math.round(value * 1000) / 1000;
      return {
        name,
        n: sorted.length,
        meanMs: round(total / sorted.length),
        p50Ms: round(percentile(sorted, 0.5)),
        p95Ms: round(percentile(sorted, 0.95)),
        maxMs: round(sorted[sorted.length - 1]),
      };
    })
    .sort((left, right) => left.name.localeCompare(right.name));
}

function scheduleFlush(): void {
  if (flushTimer !== undefined || pending.length === 0) return;
  flushTimer = setTimeout(() => {
    flushTimer = undefined;
    void flushPerfProbe();
  }, FLUSH_INTERVAL_MS);
}

/** Writes buffered samples plus a rolling summary. Failures are never fatal. */
export async function flushPerfProbe(): Promise<void> {
  if (!enabled || !appender) return;
  const lines = pending;
  pending = [];
  const summary = perfSummary();
  if (summary.length) lines.push(JSON.stringify({ t: Date.now(), summary }));
  if (lines.length === 0) return;
  try {
    await appender(lines);
  } catch {
    // Instrumentation must never break the app it is measuring.
  }
}
