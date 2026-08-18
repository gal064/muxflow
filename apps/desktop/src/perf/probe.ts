import perfLogContract from "../../perf-log-contract.json";

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
/** Native append command limit; every frontend batch must fit independently. */
const MAX_APPEND_LINES = perfLogContract.maxLinesPerAppend;
const MAX_LINE_BYTES = perfLogContract.maxLineBytes;
const MAX_RECORD_PAYLOAD_BYTES = MAX_LINE_BYTES - 80;
/** Bound unsaved renderer memory while retaining an explicit invalidity record. */
const MAX_PENDING_LINES = MAX_APPEND_LINES * 4;
const jsonByteEncoder = new TextEncoder();

type Appender = (lines: string[]) => Promise<void>;

const samples = new Map<string, number[]>();
const counters = new Map<string, number>();
const highWater = new Map<string, number>();
const milestoneOccurrences = new Map<string, number>();
const panePaintSpans = new Map<number, { name: PanePaintSpan; scopeId: string; started: number; targetPaneId?: string }>();
const recentPanePaints = new Map<string, number>();
let nextPanePaintSpanToken = 0;
let recordIdPrefix = "";
let nextRecordId = 0;
const MAX_PENDING_PANE_PAINT_SPANS = 256;
const PANE_PAINT_SPAN_TIMEOUT_MS = 30_000;
let pending: string[] = [];
let aggregatePending: string[] = [];
let retryBatch: string[] | undefined;
const sinkInvalidCounts = new Map<string, number>();
let enabled = false;
let appender: Appender | undefined;
let flushTimer: ReturnType<typeof setTimeout> | undefined;
let longTaskObserver: PerformanceObserver | undefined;
let operationSnapshotDirty = false;
let summarySnapshotDirty = false;
let flushInFlight: Promise<void> | undefined;

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
  recordIdPrefix = crypto.randomUUID();
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
  panePaintSpans.clear();
  recentPanePaints.clear();
  nextPanePaintSpanToken = 0;
  recordIdPrefix = "";
  nextRecordId = 0;
  pending = [];
  aggregatePending = [];
  retryBatch = undefined;
  sinkInvalidCounts.clear();
  operationSnapshotDirty = false;
  summarySnapshotDirty = false;
  flushInFlight = undefined;
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
  operationSnapshotDirty = true;
  scheduleFlush();
}

/** Computes JSON payload size only after proving the opt-in probe is enabled. */
export function recordPerfJsonBytes(name: string, value: unknown): void {
  if (!enabled) return;
  try {
    recordPerfCounter(name, jsonByteEncoder.encode(JSON.stringify(value)).byteLength);
  } catch {
    recordPerfCounter(`${name}.measurementFailures`);
  }
}

/** Keeps response serialization outside every latency span in the current task. */
export function recordPerfJsonBytesDeferred(name: string, value: unknown): void {
  if (!enabled) return;
  setTimeout(() => recordPerfJsonBytes(name, value), 0);
}

export function recordPerfHighWater(name: string, value: number): void {
  if (!enabled || !Number.isSafeInteger(value) || value < 0) return;
  if (value <= (highWater.get(name) ?? -1)) return;
  highWater.set(name, value);
  operationSnapshotDirty = true;
  scheduleFlush();
}

/** Records an ordered startup/interaction milestone and its occurrence. */
export function recordPerfMilestone(name: string, atMs = now()): number {
  if (!enabled) return 0;
  const occurrence = (milestoneOccurrences.get(name) ?? 0) + 1;
  milestoneOccurrences.set(name, occurrence);
  enqueueLine(JSON.stringify({ t: Date.now(), kind: "milestone", name, occurrence, atMs }));
  scheduleFlush();
  return occurrence;
}

export function perfCounterSnapshot(): Record<string, number> {
  return Object.fromEntries([...counters.entries()].sort(([left], [right]) => left.localeCompare(right)));
}

export function perfHighWaterSnapshot(): Record<string, number> {
  return Object.fromEntries([...highWater.entries()].sort(([left], [right]) => left.localeCompare(right)));
}

/**
 * `fields` are stamped onto the emitted record only — an `operationId` lets
 * the segments of one operation be joined — and never join the summary key.
 */
export function recordPerfSample(name: string, milliseconds: number, fields?: Record<string, unknown>): void {
  if (!enabled || !Number.isFinite(milliseconds) || milliseconds < 0) return;
  const bucket = samples.get(name) ?? [];
  bucket.push(milliseconds);
  if (bucket.length > MAX_SAMPLES) bucket.shift();
  samples.set(name, bucket);
  summarySnapshotDirty = true;
  enqueueLine(JSON.stringify({ t: Date.now(), name, ms: Math.round(milliseconds * 1000) / 1000, ...fields }));
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

/** Completes a measurement after React has committed and the browser has painted. */
export function afterNextPaint(work: () => void): void {
  if (!enabled) return;
  if (typeof requestAnimationFrame === "undefined") {
    queueMicrotask(work);
    return;
  }
  requestAnimationFrame(() => requestAnimationFrame(work));
}

/**
 * User interactions whose completion the user perceives as "the terminal is
 * showing me something": each ends when a pane finishes painting its first
 * content, wherever in the tree that happens.
 */
export const PANE_PAINT_SPANS = ["create.tab", "create.workspace", "pane.split", "window.switch"] as const;

export type PanePaintSpan = (typeof PANE_PAINT_SPANS)[number];

export type PanePaintSpanToken = number;

/** Opens a concurrent action span; its exact pane target is bound by the authoritative ack. */
export function openPanePaintSpan(name: PanePaintSpan, scopeId: string): PanePaintSpanToken | undefined {
  if (!enabled) return undefined;
  prunePanePaintSpans(now());
  while (panePaintSpans.size >= MAX_PENDING_PANE_PAINT_SPANS) {
    panePaintSpans.delete(panePaintSpans.keys().next().value as number);
    recordPerfCounter("workflow.panePaintSpanCapacityDrops");
  }
  const token = ++nextPanePaintSpanToken;
  panePaintSpans.set(token, { name, scopeId, started: now() });
  return token;
}

export function targetPanePaintSpan(token: PanePaintSpanToken | undefined, paneId: string | undefined): void {
  if (token === undefined) return;
  const span = panePaintSpans.get(token);
  if (!span || !paneId) {
    panePaintSpans.delete(token);
    return;
  }
  span.targetPaneId = paneId;
  const paintedAt = recentPanePaints.get(panePaintKey(span.scopeId, paneId));
  if (paintedAt !== undefined && paintedAt >= span.started) finishPanePaintSpan(token, span, paintedAt);
}

export function abandonPanePaintSpan(token: PanePaintSpanToken | undefined): void {
  if (token !== undefined) panePaintSpans.delete(token);
}

export function abandonPanePaintSpansForScope(scopeId: string | undefined): void {
  if (!scopeId) return;
  for (const [token, span] of panePaintSpans) {
    if (span.scopeId === scopeId) panePaintSpans.delete(token);
  }
  for (const key of recentPanePaints.keys()) {
    if (key.startsWith(`${scopeId}\0`)) recentPanePaints.delete(key);
  }
}

export function closePanePaintSpans(scopeId: string | undefined, paneId: string): void {
  if (!enabled) return;
  const paintedAt = now();
  prunePanePaintSpans(paintedAt);
  if (!scopeId) return;
  const key = panePaintKey(scopeId, paneId);
  recentPanePaints.delete(key);
  recentPanePaints.set(key, paintedAt);
  while (recentPanePaints.size > 256) recentPanePaints.delete(recentPanePaints.keys().next().value as string);
  for (const [token, span] of panePaintSpans) {
    if (span.scopeId === scopeId && span.targetPaneId === paneId) finishPanePaintSpan(token, span, paintedAt);
  }
}

function finishPanePaintSpan(token: PanePaintSpanToken, span: { name: PanePaintSpan; started: number }, paintedAt: number): void {
  panePaintSpans.delete(token);
  recordPerfSample(span.name, Math.max(0, paintedAt - span.started));
}

function panePaintKey(scopeId: string, paneId: string): string {
  return `${scopeId}\0${paneId}`;
}

function prunePanePaintSpans(atMs: number): void {
  for (const [token, span] of panePaintSpans) {
    if (atMs - span.started < PANE_PAINT_SPAN_TIMEOUT_MS) continue;
    panePaintSpans.delete(token);
    recordPerfCounter("workflow.panePaintSpanTimeouts");
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

/**
 * Times only a successful response under the requested name and accounts for
 * rejected/cancelled attempts separately. Use this for request/ack semantics;
 * `measurePerf` remains appropriate when failure latency itself is the sample.
 */
export async function measurePerfOutcome<T>(name: string, work: () => Promise<T>): Promise<T> {
  if (!enabled) return work();
  recordPerfCounter(`${name}.attempts`);
  const started = now();
  try {
    const result = await work();
    recordPerfCounter(`${name}.successes`);
    recordPerfSample(name, now() - started);
    return result;
  } catch (error) {
    const cancelled = error instanceof Error && error.name === "AbortError";
    recordPerfCounter(`${name}.${cancelled ? "cancellations" : "failures"}`);
    recordPerfSample(`${name}.${cancelled ? "cancelled" : "failed"}`, now() - started);
    throw error;
  }
}

/**
 * Canonical renderer-to-native request boundary. The named series contains
 * successful, validated response latency only; global and domain counters
 * share the same outcome decision, so their totals cannot drift by caller.
 */
export async function measurePerfRequest<T, Boundary>(
  name: string,
  domain: string,
  boundary: Boundary,
  work: (boundary: Boundary) => Promise<T>,
  request?: { encoding?: "json" | "raw"; byteCounters?: readonly string[] },
): Promise<T> {
  if (!enabled) return work(boundary);
  const prefixes = ["desktop.hostRequest", `${domain}.hostRequest`];
  let requestBytes: number | undefined;
  if (request?.encoding === "raw") {
    requestBytes = boundary instanceof ArrayBuffer || ArrayBuffer.isView(boundary)
      ? boundary.byteLength
      : undefined;
  } else {
    try {
      requestBytes = jsonByteEncoder.encode(JSON.stringify(boundary)).byteLength;
    } catch {
      for (const prefix of prefixes) recordPerfCounter(`${prefix}ByteMeasurementFailures`);
    }
  }
  if (requestBytes !== undefined && Number.isSafeInteger(requestBytes) && requestBytes >= 0) {
    for (const prefix of prefixes) recordPerfCounter(`${prefix}Bytes`, requestBytes);
    for (const counter of request?.byteCounters ?? []) recordPerfCounter(counter, requestBytes);
  }
  recordPerfCounter(`${name}.attempts`);
  for (const prefix of prefixes) recordPerfCounter(`${prefix}Attempts`);
  const started = now();
  try {
    const result = await work(boundary);
    recordPerfCounter(`${name}.successes`);
    for (const prefix of prefixes) recordPerfCounter(`${prefix}Successes`);
    recordPerfSample(name, now() - started);
    return result;
  } catch (error) {
    const cancelled = error instanceof Error && error.name === "AbortError";
    const outcome = cancelled ? "Cancellations" : "Failures";
    recordPerfCounter(`${name}.${cancelled ? "cancellations" : "failures"}`);
    for (const prefix of prefixes) recordPerfCounter(`${prefix}${outcome}`);
    recordPerfSample(`${name}.${cancelled ? "cancelled" : "failed"}`, now() - started);
    throw error;
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
  if (flushTimer !== undefined || !hasBufferedEvidence()) return;
  flushTimer = setTimeout(() => {
    flushTimer = undefined;
    void flushPerfProbe();
  }, FLUSH_INTERVAL_MS);
}

/** Writes buffered samples plus a rolling summary. Failures are never fatal. */
export function flushPerfProbe(): Promise<void> {
  if (flushInFlight) return flushInFlight;
  flushInFlight = drainPerfProbe().finally(() => {
    flushInFlight = undefined;
    if (hasBufferedEvidence()) scheduleFlush();
  });
  return flushInFlight;
}

async function drainPerfProbe(): Promise<void> {
  if (!enabled || !appender) return;
  while (hasBufferedEvidence()) {
    const lines = retryBatch ?? takeBatch();
    if (lines.length === 0) return;
    try {
      await appender(lines);
      if (retryBatch === lines) retryBatch = undefined;
    } catch {
      // Keep one immutable, native-size-bounded retry batch separate from new
      // observations. A later flush retries it byte-for-byte without growing a
      // summary or stranding records that arrived during the failed append.
      retryBatch = lines;
      return;
    }
  }
}

function enqueueLine(line: string): void {
  line = identifyRecord(line);
  if (jsonByteEncoder.encode(line).byteLength > MAX_LINE_BYTES) {
    recordSinkInvalid("renderer record exceeds UTF-8 byte limit");
    return;
  }
  if (line.includes("\n") || line.includes("\r")) {
    recordSinkInvalid("renderer record contains a line break");
    return;
  }
  if (pending.length >= MAX_PENDING_LINES) {
    recordSinkInvalid("renderer pending line limit exceeded");
    return;
  }
  pending.push(line);
}

function hasBufferedEvidence(): boolean {
  return Boolean(
    retryBatch?.length
    || pending.length
    || aggregatePending.length
    || sinkInvalidCounts.size
    || operationSnapshotDirty
    || summarySnapshotDirty,
  );
}

function takeBatch(): string[] {
  const lines = pending.splice(0, MAX_APPEND_LINES);
  while (lines.length < MAX_APPEND_LINES && sinkInvalidCounts.size > 0) {
    const [reason, droppedLines] = sinkInvalidCounts.entries().next().value as [string, number];
    sinkInvalidCounts.delete(reason);
    lines.push(identifyRecord(JSON.stringify({ t: Date.now(), kind: "sinkInvalid", reason, droppedLines })));
  }
  if (lines.length < MAX_APPEND_LINES && aggregatePending.length === 0) prepareAggregateLines();
  lines.push(...aggregatePending.splice(0, MAX_APPEND_LINES - lines.length));
  return lines;
}

function prepareAggregateLines(): void {
  const timestamp = Date.now();
  if (operationSnapshotDirty) {
    operationSnapshotDirty = false;
    aggregatePending.push(...chunkMapSnapshot(timestamp, perfCounterSnapshot(), perfHighWaterSnapshot()));
  }
  if (summarySnapshotDirty) {
    summarySnapshotDirty = false;
    aggregatePending.push(...chunkRows(timestamp, "summary", perfSummary()));
  }
}

function chunkMapSnapshot(
  timestamp: number,
  counterSnapshot: Record<string, number>,
  highWaterSnapshot: Record<string, number>,
): string[] {
  const rows = [
    ...Object.entries(counterSnapshot).map(([name, value]) => ({ group: "counters" as const, name, value })),
    ...Object.entries(highWaterSnapshot).map(([name, value]) => ({ group: "highWater" as const, name, value })),
  ];
  const records: string[] = [];
  let counters: Record<string, number> = {};
  let highWater: Record<string, number> = {};
  for (const row of rows) {
    const target = row.group === "counters" ? counters : highWater;
    target[row.name] = row.value;
    const candidate = JSON.stringify({ t: timestamp, kind: "operations", counters, highWater });
    if (jsonByteEncoder.encode(candidate).byteLength <= MAX_RECORD_PAYLOAD_BYTES) continue;
    delete target[row.name];
    pushBoundedAggregate(records, JSON.stringify({ t: timestamp, kind: "operations", counters, highWater }));
    counters = row.group === "counters" ? { [row.name]: row.value } : {};
    highWater = row.group === "highWater" ? { [row.name]: row.value } : {};
  }
  if (Object.keys(counters).length || Object.keys(highWater).length) {
    pushBoundedAggregate(records, JSON.stringify({ t: timestamp, kind: "operations", counters, highWater }));
  }
  return records;
}

function chunkRows(timestamp: number, kind: "summary", rows: readonly PerfSummaryRow[]): string[] {
  const records: string[] = [];
  let chunk: PerfSummaryRow[] = [];
  for (const row of rows) {
    const candidate = JSON.stringify({ t: timestamp, kind, summary: [...chunk, row] });
    if (jsonByteEncoder.encode(candidate).byteLength <= MAX_RECORD_PAYLOAD_BYTES) {
      chunk.push(row);
      continue;
    }
    if (chunk.length) pushBoundedAggregate(records, JSON.stringify({ t: timestamp, kind, summary: chunk }));
    chunk = [row];
  }
  if (chunk.length) pushBoundedAggregate(records, JSON.stringify({ t: timestamp, kind, summary: chunk }));
  return records;
}

function pushBoundedAggregate(records: string[], record: string): void {
  const identified = identifyRecord(record);
  if (jsonByteEncoder.encode(identified).byteLength <= MAX_LINE_BYTES) records.push(identified);
  else recordSinkInvalid("aggregate row exceeds UTF-8 byte limit");
}

function recordSinkInvalid(reason: string): void {
  sinkInvalidCounts.set(reason, (sinkInvalidCounts.get(reason) ?? 0) + 1);
}

function identifyRecord(line: string): string {
  if (!recordIdPrefix || !line.endsWith("}")) return line;
  nextRecordId += 1;
  return `${line.slice(0, -1)},"recordId":"${recordIdPrefix}:${nextRecordId}"}`;
}
