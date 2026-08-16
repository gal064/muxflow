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
const counters = new Map<string, number>();
const highWater = new Map<string, number>();
const milestoneOccurrences = new Map<string, number>();
export interface PerfSpanHandle {
  readonly name: string;
  readonly scope?: string;
  readonly started: number;
}

interface OpenPerfSpan {
  readonly started: number;
  readonly owners: Set<PerfSpanHandle>;
}

const openSpans = new Map<string, OpenPerfSpan>();
const perfSpanKey = (name: string, scope?: string) => `${name}\0${scope ?? ""}`;
let pending: string[] = [];
let enabled = false;
let appender: Appender | undefined;
let flushTimer: ReturnType<typeof setTimeout> | undefined;
let longTaskObserver: PerformanceObserver | undefined;

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
  if (typeof PerformanceObserver !== "undefined") {
    try {
      longTaskObserver = new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) {
          recordPerfCounter("react.longTasks");
          recordPerfSample("react.longTask", entry.duration);
        }
      });
      longTaskObserver.observe({ entryTypes: ["longtask"] });
    } catch {
      longTaskObserver = undefined;
    }
  }
}

export function perfProbeEnabled(): boolean {
  return enabled;
}

export function resetPerfProbe(): void {
  enabled = false;
  appender = undefined;
  samples.clear();
  counters.clear();
  highWater.clear();
  milestoneOccurrences.clear();
  openSpans.clear();
  pending = [];
  if (flushTimer !== undefined) clearTimeout(flushTimer);
  longTaskObserver?.disconnect();
  longTaskObserver = undefined;
  flushTimer = undefined;
}

/**
 * Deterministic Phase 14 operation accounting. Unlike a duration sample these
 * values are suitable for unit gates: a refactor either performed an extra
 * copy/list/watch/queue operation or it did not. They share the Phase 12 sink
 * and are completely inert until that sink is explicitly enabled.
 */
export function recordPerfCounter(name: string, delta = 1): void {
  if (!enabled || !Number.isSafeInteger(delta)) return;
  const value = (counters.get(name) ?? 0) + delta;
  counters.set(name, value);
  pending.push(JSON.stringify({ t: Date.now(), kind: "counter", name, delta, value }));
  scheduleFlush();
}

export function recordPerfHighWater(name: string, value: number): void {
  if (!enabled || !Number.isSafeInteger(value) || value < 0) return;
  if (value <= (highWater.get(name) ?? -1)) return;
  highWater.set(name, value);
  pending.push(JSON.stringify({ t: Date.now(), kind: "highWater", name, value }));
  scheduleFlush();
}

/** Records an ordered startup/interaction milestone and its occurrence. */
export function recordPerfMilestone(name: string, atMs = now()): number {
  if (!enabled) return 0;
  const occurrence = (milestoneOccurrences.get(name) ?? 0) + 1;
  milestoneOccurrences.set(name, occurrence);
  pending.push(JSON.stringify({ t: Date.now(), kind: "milestone", name, occurrence, atMs }));
  scheduleFlush();
  return occurrence;
}

export function perfCounterSnapshot(): Record<string, number> {
  return Object.fromEntries([...counters.entries()].sort(([left], [right]) => left.localeCompare(right)));
}

export function perfHighWaterSnapshot(): Record<string, number> {
  return Object.fromEntries([...highWater.entries()].sort(([left], [right]) => left.localeCompare(right)));
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
export function openPerfSpan(name: string, scope?: string): PerfSpanHandle | undefined {
  if (!enabled) return undefined;
  const key = perfSpanKey(name, scope);
  const existing = openSpans.get(key);
  const started = existing?.started ?? now();
  const handle = { name, scope, started };
  if (existing) existing.owners.add(handle);
  else openSpans.set(key, { started, owners: new Set([handle]) });
  return handle;
}

/** Closes an open cross-component span. Closing an unopened span is a no-op. */
export function closePerfSpan(name: string, scope?: string): void {
  if (!enabled) return;
  const key = perfSpanKey(name, scope);
  const span = openSpans.get(key);
  if (!span) return;
  openSpans.delete(key);
  recordPerfSample(name, now() - span.started);
}

export function abandonPerfSpan(name: string, owner?: PerfSpanHandle): void {
  const key = perfSpanKey(name, owner?.scope);
  const span = openSpans.get(key);
  if (!span) return;
  if (owner) {
    if (!span.owners.delete(owner) || span.owners.size > 0) return;
  }
  openSpans.delete(key);
}

/**
 * User interactions whose completion the user perceives as "the terminal is
 * showing me something": each ends when a pane finishes painting its first
 * content, wherever in the tree that happens.
 */
export const PANE_PAINT_SPANS = ["create.tab", "create.workspace", "pane.split", "window.switch"] as const;

export type PanePaintSpan = (typeof PANE_PAINT_SPANS)[number];

export function closePanePaintSpans(scope: string): void {
  for (const name of PANE_PAINT_SPANS) closePerfSpan(name, scope);
}

/** A replaced connection can never paint the pane an old action was waiting for. */
export function abandonPanePaintSpans(scope: string): void {
  for (const name of PANE_PAINT_SPANS) {
    openSpans.delete(perfSpanKey(name, scope));
  }
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
