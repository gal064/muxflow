import { recordIncident } from "../../diagnostics/incidents";

/**
 * Attributes a slow paint to the mechanism that caused it.
 *
 * `inputLatencyStats` reports that the paint segment has a tail — a p50 of one
 * millisecond with rare samples of a quarter second — and a histogram cannot
 * say *why*. There are exactly three candidates behind an xterm write
 * completion that took three frames to fire, and each leaves a different mark:
 *
 * - **Backlog.** xterm queues chunks and parses them in time-sliced batches, so
 *   our callback waits behind whatever was already queued. Marked by a high
 *   `queuedWrites`/`queuedBytes` with no long task in the window.
 * - **Main-thread blockage.** One long task — a React render, a layout, a GC —
 *   owned the loop while our chunk waited. Marked by `longTasks`.
 * - **Atlas work.** The shared glyph atlas was invalidated mid-window and the
 *   texture had to be rebuilt. Marked by `atlasDelta > 0`.
 *
 * All three fingerprints ride on one `render.paintSlow` record, so a single
 * journal line settles the question instead of a second measurement campaign.
 *
 * Always on, and priced for it: a paint under the threshold costs one
 * comparison, and a slow one costs a walk of a 32-entry ring. Rate limited per
 * pane, because the interesting fact about a pane painting slowly for a whole
 * minute is that it happened, not that it happened four hundred times — and the
 * count of what the limit swallowed rides on the next record so the silence
 * stays countable. Nothing here can throw into the write path.
 */

/**
 * How slow a write completion has to be before it is worth a journal line.
 *
 * Three frames at 60Hz. Below this a paint is inside the budget the user cannot
 * see past, and the tail this exists to explain starts an order of magnitude
 * above it.
 */
export const PAINT_SLOW_THRESHOLD_MS = 48;

/** How often one pane may contribute a slow-paint record. */
export const PAINT_SLOW_INCIDENT_INTERVAL_MS = 5_000;

/**
 * How many long tasks are kept. The window a paint asks about is tens of
 * milliseconds wide and a long task is at least fifty, so the last handful is
 * always enough; the bound is what keeps an hour of jank from growing an array.
 */
const LONG_TASK_RING_SIZE = 32;

interface LongTaskEntry {
  startTime: number;
  duration: number;
}

/**
 * What the long tasks intersecting one paint window add up to.
 *
 * `totalMs` and `maxMs` are the intersecting tasks' own durations, not the part
 * of them that fell inside the window: a 300ms task overlapping a 50ms paint is
 * reported as 300, because the size of the blockage is the finding.
 */
export interface LongTaskOverlap {
  count: number;
  totalMs: number;
  maxMs: number;
}

export interface PaintSample {
  paneId: string;
  /** How long the write completion took, in milliseconds. */
  ms: number;
  /** `performance.now()` at the moment the bytes were handed to the renderer. */
  startedAtMs: number;
  bytes: number;
  /** Our writes still in xterm's buffer, including this one. */
  queuedWrites: number;
  queuedBytes: number;
  /** How far the shared atlas's invalidation counter moved across the write. */
  atlasDelta: number | undefined;
}

interface PaneState {
  /** When this pane last emitted, on the same clock as `startedAtMs`. */
  lastRecordAtMs: number | undefined;
  /** Slow paints dropped by the rate limit since the last emitted record. */
  suppressed: number;
}

const ring: (LongTaskEntry | undefined)[] = new Array<LongTaskEntry | undefined>(LONG_TASK_RING_SIZE).fill(undefined);
let ringNext = 0;
let observer: { disconnect: () => void } | undefined;
/** Whether the long-task API is present and observing. */
let tracking = false;
/** Whether installation has been attempted; absence is permanent, so try once. */
let attempted = false;
const panes = new Map<string, PaneState>();

function remember(entry: LongTaskEntry): void {
  ring[ringNext] = entry;
  ringNext = (ringNext + 1) % LONG_TASK_RING_SIZE;
}

/**
 * Installs the long-task observer, once per app launch.
 *
 * `longtask` is not in every WebView, and an absent API must cost the record
 * its long-task field and cost the app nothing — so construction and `observe`
 * are both guarded, and a failure leaves `tracking` false forever rather than
 * retrying on every pane mount.
 */
export function startLongTaskTracker(): void {
  if (attempted) return;
  attempted = true;
  try {
    const observerCtor = (globalThis as {
      PerformanceObserver?: new (callback: (list: { getEntries: () => LongTaskEntry[] }) => void) => {
        observe: (options: { type: string; buffered: boolean }) => void;
        disconnect: () => void;
      };
    }).PerformanceObserver;
    if (typeof observerCtor !== "function") return;
    const created = new observerCtor((list) => {
      try {
        for (const entry of list.getEntries()) {
          remember({ startTime: entry.startTime, duration: entry.duration });
        }
      } catch {
        // A malformed entry list loses one batch of long tasks and nothing else.
      }
    });
    created.observe({ type: "longtask", buffered: false });
    observer = created;
    tracking = true;
  } catch {
    // No `longtask` support here. Every fingerprint field goes out undefined.
    observer = undefined;
    tracking = false;
  }
}

/** Stops observing and forgets what was seen. Test seam, and a clean teardown. */
export function disposeLongTaskTracker(): void {
  try {
    observer?.disconnect();
  } catch {
    // Nothing to do about an observer that refuses to stop.
  }
  observer = undefined;
  tracking = false;
  attempted = false;
  ring.fill(undefined);
  ringNext = 0;
}

/**
 * The long tasks intersecting `[startMs, endMs]`, or `undefined` when this
 * WebView has no long-task API to ask — which is not the same fact as "no long
 * task ran", and must not be reported as if it were.
 */
export function longTasksOverlapping(startMs: number, endMs: number): LongTaskOverlap | undefined {
  if (!tracking) return undefined;
  let count = 0;
  let totalMs = 0;
  let maxMs = 0;
  for (const entry of ring) {
    if (!entry) continue;
    if (entry.startTime >= endMs) continue;
    if (entry.startTime + entry.duration <= startMs) continue;
    count += 1;
    totalMs += entry.duration;
    if (entry.duration > maxMs) maxMs = entry.duration;
  }
  return { count, totalMs: Math.round(totalMs), maxMs: Math.round(maxMs) };
}

function stateFor(paneId: string): PaneState {
  const existing = panes.get(paneId);
  if (existing) return existing;
  const created: PaneState = { lastRecordAtMs: undefined, suppressed: 0 };
  panes.set(paneId, created);
  return created;
}

/**
 * Considers one write completion, and journals it when it was slow enough to
 * explain. Called on every live-output write, so the fast path is the one that
 * matters: a comparison and a return.
 */
export function notePaint(input: PaintSample): void {
  try {
    const { ms } = input;
    if (!Number.isFinite(ms) || ms < PAINT_SLOW_THRESHOLD_MS) return;
    const startedAtMs = Number.isFinite(input.startedAtMs) ? input.startedAtMs : 0;
    const state = stateFor(input.paneId);
    if (state.lastRecordAtMs !== undefined && startedAtMs - state.lastRecordAtMs < PAINT_SLOW_INCIDENT_INTERVAL_MS) {
      // Dropped, not deferred: holding the slowest sample of an interval would
      // mean a timer and a pending write to flush it, and the count of what was
      // dropped is what the next record needs to be read honestly.
      state.suppressed += 1;
      return;
    }
    const suppressed = state.suppressed;
    state.lastRecordAtMs = startedAtMs;
    state.suppressed = 0;
    recordIncident("render.paintSlow", {
      paneId: input.paneId,
      ms: Math.round(ms),
      bytes: input.bytes,
      queuedWrites: input.queuedWrites,
      queuedBytes: input.queuedBytes,
      atlasDelta: input.atlasDelta,
      longTasks: longTasksOverlapping(startedAtMs, startedAtMs + ms),
      suppressed,
    });
  } catch {
    // A probe that throws into a write completion is worse than a missing line.
  }
}

/** Test seam. Feeds the ring without needing a real `longtask` observer. */
export function __noteLongTaskForTests(entry: LongTaskEntry): void {
  attempted = true;
  tracking = true;
  remember(entry);
}

/** Test seam. Resets the module to the state a fresh app launch starts in. */
export function __resetPaintTailProbeForTests(): void {
  disposeLongTaskTracker();
  panes.clear();
}
